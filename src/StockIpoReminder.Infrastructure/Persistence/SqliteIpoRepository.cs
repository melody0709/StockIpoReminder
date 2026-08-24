using System.Globalization;
using System.Text.Json;
using Microsoft.Data.Sqlite;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Persistence;

public sealed class SqliteIpoRepository : IIpoRepository
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = false,
    };

    private readonly DatabaseOptions _options;
    private readonly ILogger<SqliteIpoRepository> _logger;
    private readonly TimeProvider _timeProvider;

    public SqliteIpoRepository(
        DatabaseOptions options,
        ILogger<SqliteIpoRepository> logger,
        TimeProvider timeProvider)
    {
        _options = options;
        _logger = logger;
        _timeProvider = timeProvider;
    }

    public async Task InitializeAsync(CancellationToken cancellationToken = default)
    {
        var directory = Path.GetDirectoryName(_options.DatabasePath);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = MigrationSql;
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<IpoEvent?> GetEventAsync(string id, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT * FROM ipo_events WHERE id = $id LIMIT 1;";
        command.Parameters.AddWithValue("$id", id);
        IpoEvent? ipoEvent;
        await using (var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false))
        {
            ipoEvent = await reader.ReadAsync(cancellationToken).ConfigureAwait(false) ? ReadEvent(reader) : null;
        }

        return ipoEvent is null
            ? null
            : await ApplyManualOverridesAsync(connection, transaction: null, ipoEvent, cancellationToken).ConfigureAwait(false);
    }

    public async Task<IpoEvent?> GetPublicEventAsync(string id, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        return await GetEventAsync(connection, transaction: null!, id, cancellationToken).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<IpoEvent>> GetEventsAsync(DateOnly from, DateOnly to, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            SELECT * FROM ipo_events e
            WHERE e.apply_date BETWEEN $from AND $to
               OR EXISTS(
                    SELECT 1 FROM manual_overrides m
                    WHERE m.ipo_event_id = e.id
                      AND m.event_version = e.event_version
                      AND m.revoked_at IS NULL
                      AND m.field_name = 'ApplyDate'
                      AND m.override_value BETWEEN $from AND $to)
            ORDER BY apply_date, exchange, security_code;
            """;
        command.Parameters.AddWithValue("$from", Format(from));
        command.Parameters.AddWithValue("$to", Format(to));
        var rawEvents = await ReadEventsAsync(command, cancellationToken).ConfigureAwait(false);
        var effective = new List<IpoEvent>(rawEvents.Count);
        foreach (var ipoEvent in rawEvents)
        {
            var applied = await ApplyManualOverridesAsync(connection, transaction: null, ipoEvent, cancellationToken).ConfigureAwait(false);
            if (applied.ApplyDate is not null && applied.ApplyDate >= from && applied.ApplyDate <= to)
            {
                effective.Add(applied);
            }
        }

        return effective
            .OrderBy(static item => item.ApplyDate)
            .ThenBy(static item => item.Exchange)
            .ThenBy(static item => item.SecurityCode, StringComparer.Ordinal)
            .ToArray();
    }

    public async Task<IReadOnlyList<IpoEvent>> GetPendingEventsAsync(DateOnly date, CancellationToken cancellationToken = default)
    {
        var events = await GetEventsAsync(date, date, cancellationToken).ConfigureAwait(false);
        return events
            .Where(static item => (item.LifecycleStatus is IpoLifecycleStatus.Scheduled
                    or IpoLifecycleStatus.ActiveUnconfirmed
                    or IpoLifecycleStatus.AcknowledgedNeedsReview)
                && item.Status is not IssueStatus.Suspended and not IssueStatus.Terminated)
            .ToArray();
    }

    public async Task<UpsertEventResult> UpsertEventAsync(ReconciledIpoEvent resolved, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        var existing = await GetEventAsync(connection, transaction, resolved.Event.Id, cancellationToken).ConfigureAwait(false);
        var incoming = existing is null ? resolved.Event : MergeWithExisting(existing, resolved.Event);
        var changed = existing is null ? [] : FindChangedFields(existing, incoming);
        var criticalChanged = changed.Any(IsCriticalField);
        var version = existing?.EventVersion ?? incoming.EventVersion;
        if (existing is not null && criticalChanged)
        {
            version++;
        }

        var lifecycle = incoming.LifecycleStatus;
        if (existing?.LifecycleStatus is IpoLifecycleStatus.Acknowledged or IpoLifecycleStatus.AcknowledgedNeedsReview)
        {
            lifecycle = criticalChanged
                ? IpoLifecycleStatus.AcknowledgedNeedsReview
                : existing.LifecycleStatus;
        }

        var persisted = incoming with
        {
            EventVersion = version,
            LifecycleStatus = lifecycle,
            FirstSeenAt = existing?.FirstSeenAt ?? resolved.Event.FirstSeenAt,
        };

        await UpsertEventRowAsync(connection, transaction, persisted, cancellationToken).ConfigureAwait(false);
        await ReplaceFieldSourcesAsync(connection, transaction, persisted.Id, resolved.FieldSources, cancellationToken).ConfigureAwait(false);

        if (criticalChanged)
        {
            await using var cancel = connection.CreateCommand();
            cancel.Transaction = transaction;
            cancel.CommandText = """
                UPDATE reminder_outbox
                SET delivery_state = $cancelled, updated_at = $now
                WHERE ipo_event_id = $id
                  AND delivery_state IN ($pending, $leased);
                """;
            cancel.Parameters.AddWithValue("$cancelled", (int)ReminderDeliveryState.Cancelled);
            cancel.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
            cancel.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
            cancel.Parameters.AddWithValue("$now", Format(persisted.UpdatedAt));
            cancel.Parameters.AddWithValue("$id", persisted.Id);
            await cancel.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
        return new UpsertEventResult
        {
            Event = persisted,
            Created = existing is null,
            EventVersionChanged = existing is not null && version != existing.EventVersion,
            CriticalFieldsChanged = criticalChanged,
            ChangedFields = changed,
        };
    }

    public async Task SaveCollectorResultAsync(CollectorResult result, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);

        await using (var raw = connection.CreateCommand())
        {
            raw.Transaction = transaction;
            raw.CommandText = """
                INSERT INTO raw_payloads(source, fetched_at, success, record_count, raw_hash, schema_fingerprint, payload, error)
                VALUES($source, $fetched, $success, $count, $hash, $schema, $payload, $error);
                """;
            raw.Parameters.AddWithValue("$source", result.Source);
            raw.Parameters.AddWithValue("$fetched", Format(result.FinishedAt));
            raw.Parameters.AddWithValue("$success", result.Success ? 1 : 0);
            raw.Parameters.AddWithValue("$count", result.RecordCount);
            raw.Parameters.AddWithValue("$hash", DbValue(result.RawHash));
            raw.Parameters.AddWithValue("$schema", DbValue(result.SchemaFingerprint));
            raw.Parameters.AddWithValue("$payload", DbValue(result.RawPayload));
            raw.Parameters.AddWithValue("$error", DbValue(result.Error));
            await raw.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using (var run = connection.CreateCommand())
        {
            run.Transaction = transaction;
            run.CommandText = """
                INSERT INTO sync_runs(source, started_at, finished_at, success, record_count, error)
                VALUES($source, $started, $finished, $success, $count, $error);
                """;
            run.Parameters.AddWithValue("$source", result.Source);
            run.Parameters.AddWithValue("$started", Format(result.StartedAt));
            run.Parameters.AddWithValue("$finished", Format(result.FinishedAt));
            run.Parameters.AddWithValue("$success", result.Success ? 1 : 0);
            run.Parameters.AddWithValue("$count", result.RecordCount);
            run.Parameters.AddWithValue("$error", DbValue(result.Error));
            await run.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using (var health = connection.CreateCommand())
        {
            health.Transaction = transaction;
            health.CommandText = """
                INSERT INTO source_health(source, last_attempt_at, last_success_at, last_record_count, schema_fingerprint,
                                          consecutive_failures, health_state, last_error)
                VALUES($source, $attempt, $successAt, $count, $schema, $failures, $state, $error)
                ON CONFLICT(source) DO UPDATE SET
                    last_attempt_at = excluded.last_attempt_at,
                    last_success_at = CASE WHEN excluded.last_success_at IS NOT NULL THEN excluded.last_success_at ELSE source_health.last_success_at END,
                    last_record_count = excluded.last_record_count,
                    schema_fingerprint = CASE WHEN excluded.schema_fingerprint IS NOT NULL THEN excluded.schema_fingerprint ELSE source_health.schema_fingerprint END,
                    consecutive_failures = CASE WHEN excluded.health_state = $healthy THEN 0 ELSE source_health.consecutive_failures + 1 END,
                    health_state = excluded.health_state,
                    last_error = excluded.last_error;
                """;
            health.Parameters.AddWithValue("$source", result.Source);
            health.Parameters.AddWithValue("$attempt", Format(result.FinishedAt));
            health.Parameters.AddWithValue("$successAt", result.Success ? Format(result.FinishedAt) : DBNull.Value);
            health.Parameters.AddWithValue("$count", result.RecordCount);
            health.Parameters.AddWithValue("$schema", DbValue(result.SchemaFingerprint));
            health.Parameters.AddWithValue("$failures", result.Success ? 0 : 1);
            health.Parameters.AddWithValue("$state", (int)(result.Success ? HealthState.Healthy : HealthState.Failed));
            health.Parameters.AddWithValue("$healthy", (int)HealthState.Healthy);
            health.Parameters.AddWithValue("$error", DbValue(result.Error));
            await health.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task SaveAnnouncementAsync(AnnouncementDocument document, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            INSERT INTO announcement_documents(
                id, ipo_event_id, provider, announcement_id, announcement_type, title, published_at,
                source_url, local_path, file_hash, extraction_status, extracted_text_hash, parser_version,
                parsed_fields_json, downloaded_at)
            VALUES($id, $eventId, $provider, $announcementId, $type, $title, $published, $url, $path,
                   $fileHash, $status, $textHash, $parserVersion, $fields, $downloaded)
            ON CONFLICT(id) DO UPDATE SET
                local_path = excluded.local_path,
                file_hash = excluded.file_hash,
                extraction_status = excluded.extraction_status,
                extracted_text_hash = excluded.extracted_text_hash,
                parser_version = excluded.parser_version,
                parsed_fields_json = excluded.parsed_fields_json,
                downloaded_at = excluded.downloaded_at;
            """;
        command.Parameters.AddWithValue("$id", document.Id);
        command.Parameters.AddWithValue("$eventId", document.IpoEventId);
        command.Parameters.AddWithValue("$provider", document.Reference.Provider);
        command.Parameters.AddWithValue("$announcementId", document.Reference.AnnouncementId);
        command.Parameters.AddWithValue("$type", DbValue(document.Reference.AnnouncementType));
        command.Parameters.AddWithValue("$title", document.Reference.Title);
        command.Parameters.AddWithValue("$published", document.Reference.PublishedAt is null ? DBNull.Value : Format(document.Reference.PublishedAt.Value));
        command.Parameters.AddWithValue("$url", document.Reference.Url.ToString());
        command.Parameters.AddWithValue("$path", document.LocalPath);
        command.Parameters.AddWithValue("$fileHash", document.FileHash);
        command.Parameters.AddWithValue("$status", (int)document.ExtractionStatus);
        command.Parameters.AddWithValue("$textHash", DbValue(document.ExtractedTextHash));
        command.Parameters.AddWithValue("$parserVersion", document.ParserVersion);
        command.Parameters.AddWithValue("$fields", JsonSerializer.Serialize(document.ParsedFields, JsonOptions));
        command.Parameters.AddWithValue("$downloaded", Format(document.DownloadedAt));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<AnnouncementDocument>> GetAnnouncementsAsync(string ipoEventId, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT * FROM announcement_documents WHERE ipo_event_id = $id ORDER BY published_at DESC, downloaded_at DESC;";
        command.Parameters.AddWithValue("$id", ipoEventId);
        var result = new List<AnnouncementDocument>();
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            var reference = new AnnouncementReference
            {
                Provider = reader.GetString(reader.GetOrdinal("provider")),
                AnnouncementId = reader.GetString(reader.GetOrdinal("announcement_id")),
                AnnouncementType = GetNullableString(reader, "announcement_type"),
                Title = reader.GetString(reader.GetOrdinal("title")),
                PublishedAt = GetNullableDateTimeOffset(reader, "published_at"),
                Url = new Uri(reader.GetString(reader.GetOrdinal("source_url"))),
            };
            result.Add(new AnnouncementDocument
            {
                Id = reader.GetString(reader.GetOrdinal("id")),
                IpoEventId = ipoEventId,
                Reference = reference,
                LocalPath = reader.GetString(reader.GetOrdinal("local_path")),
                FileHash = reader.GetString(reader.GetOrdinal("file_hash")),
                ExtractionStatus = (ExtractionStatus)reader.GetInt32(reader.GetOrdinal("extraction_status")),
                ExtractedTextHash = GetNullableString(reader, "extracted_text_hash"),
                ParserVersion = reader.GetString(reader.GetOrdinal("parser_version")),
                ParsedFields = Deserialize<ParsedAnnouncementField[]>(reader.GetString(reader.GetOrdinal("parsed_fields_json"))) ?? [],
                DownloadedAt = ParseDateTimeOffset(reader.GetString(reader.GetOrdinal("downloaded_at"))),
            });
        }

        return result;
    }

    public async Task<IReadOnlyList<SourceFieldValue>> GetFieldSourcesAsync(
        string ipoEventId,
        CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            SELECT field_name, normalized_value, raw_value, source, source_published_at,
                   fetched_at, raw_hash, priority
            FROM ipo_field_sources
            WHERE ipo_event_id = $id
            ORDER BY field_name, priority DESC, source_published_at DESC, fetched_at DESC;
            """;
        command.Parameters.AddWithValue("$id", ipoEventId);
        var result = new List<SourceFieldValue>();
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            result.Add(new SourceFieldValue
            {
                FieldName = reader.GetString(reader.GetOrdinal("field_name")),
                NormalizedValue = GetNullableString(reader, "normalized_value"),
                RawValue = GetNullableString(reader, "raw_value"),
                Source = reader.GetString(reader.GetOrdinal("source")),
                SourcePublishedAt = GetNullableDateTimeOffset(reader, "source_published_at"),
                FetchedAt = ParseDateTimeOffset(reader.GetString(reader.GetOrdinal("fetched_at"))),
                RawHash = GetNullableString(reader, "raw_hash"),
                Priority = reader.GetInt32(reader.GetOrdinal("priority")),
            });
        }

        return result;
    }

    public async Task<IReadOnlyList<ManualOverrideEntry>> GetManualOverridesAsync(
        string eventId,
        int eventVersion,
        CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        return await ReadManualOverridesAsync(connection, transaction: null, eventId, eventVersion, includeRevoked: true, cancellationToken)
            .ConfigureAwait(false);
    }

    public async Task AcknowledgeAsync(string eventId, int eventVersion, DateTimeOffset confirmedAt, string dataHash, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        await using (var ack = connection.CreateCommand())
        {
            ack.Transaction = transaction;
            ack.CommandText = """
                INSERT INTO acknowledgements(ipo_event_id, event_version, confirmed_at, confirmed_data_hash, revoked_at)
                VALUES($id, $version, $confirmed, $hash, NULL)
                ON CONFLICT(ipo_event_id, event_version) DO UPDATE SET
                    confirmed_at = excluded.confirmed_at,
                    confirmed_data_hash = excluded.confirmed_data_hash,
                    needs_review_at = NULL,
                    review_reason = NULL,
                    reconfirmed_at = CASE WHEN acknowledgements.needs_review_at IS NOT NULL THEN excluded.confirmed_at ELSE acknowledgements.reconfirmed_at END,
                    revoked_at = NULL;
                """;
            ack.Parameters.AddWithValue("$id", eventId);
            ack.Parameters.AddWithValue("$version", eventVersion);
            ack.Parameters.AddWithValue("$confirmed", Format(confirmedAt));
            ack.Parameters.AddWithValue("$hash", dataHash);
            await ack.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using (var update = connection.CreateCommand())
        {
            update.Transaction = transaction;
            update.CommandText = "UPDATE ipo_events SET lifecycle_status = $status, updated_at = $now WHERE id = $id AND event_version = $version;";
            update.Parameters.AddWithValue("$status", (int)IpoLifecycleStatus.Acknowledged);
            update.Parameters.AddWithValue("$now", Format(confirmedAt));
            update.Parameters.AddWithValue("$id", eventId);
            update.Parameters.AddWithValue("$version", eventVersion);
            await update.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using (var cancel = connection.CreateCommand())
        {
            cancel.Transaction = transaction;
            cancel.CommandText = """
                UPDATE reminder_outbox
                SET delivery_state = $cancelled, acknowledged_at = $now, updated_at = $now
                WHERE ipo_event_id = $id AND event_version = $version
                  AND delivery_state IN ($pending, $leased);
                """;
            cancel.Parameters.AddWithValue("$cancelled", (int)ReminderDeliveryState.Cancelled);
            cancel.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
            cancel.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
            cancel.Parameters.AddWithValue("$now", Format(confirmedAt));
            cancel.Parameters.AddWithValue("$id", eventId);
            cancel.Parameters.AddWithValue("$version", eventVersion);
            await cancel.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task RevokeAcknowledgementAsync(string eventId, int eventVersion, DateTimeOffset revokedAt, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        await using (var ack = connection.CreateCommand())
        {
            ack.Transaction = transaction;
            ack.CommandText = "UPDATE acknowledgements SET revoked_at = $now WHERE ipo_event_id = $id AND event_version = $version;";
            ack.Parameters.AddWithValue("$now", Format(revokedAt));
            ack.Parameters.AddWithValue("$id", eventId);
            ack.Parameters.AddWithValue("$version", eventVersion);
            await ack.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using (var update = connection.CreateCommand())
        {
            update.Transaction = transaction;
            update.CommandText = "UPDATE ipo_events SET lifecycle_status = $status, updated_at = $now WHERE id = $id AND event_version = $version;";
            update.Parameters.AddWithValue("$status", (int)IpoLifecycleStatus.ActiveUnconfirmed);
            update.Parameters.AddWithValue("$now", Format(revokedAt));
            update.Parameters.AddWithValue("$id", eventId);
            update.Parameters.AddWithValue("$version", eventVersion);
            await update.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task SetLifecycleStatusAsync(
        string eventId,
        int eventVersion,
        IpoLifecycleStatus status,
        DateTimeOffset updatedAt,
        CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            UPDATE ipo_events
            SET lifecycle_status = $status, updated_at = $updated
            WHERE id = $id AND event_version = $version;
            """;
        command.Parameters.AddWithValue("$status", (int)status);
        command.Parameters.AddWithValue("$updated", Format(updatedAt));
        command.Parameters.AddWithValue("$id", eventId);
        command.Parameters.AddWithValue("$version", eventVersion);
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task EnqueueRemindersAsync(IReadOnlyList<ReminderScheduleItem> reminders, CancellationToken cancellationToken = default)
    {
        if (reminders.Count == 0)
        {
            return;
        }

        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        foreach (var reminder in reminders)
        {
            await using var command = connection.CreateCommand();
            command.Transaction = transaction;
            command.CommandText = """
                INSERT OR IGNORE INTO reminder_outbox(
                    ipo_event_id, event_version, due_at, reminder_level, dedupe_key,
                    delivery_state, attempt_count, created_at, updated_at)
                VALUES($id, $version, $due, $level, $dedupe, $state, 0, $now, $now);
                """;
            command.Parameters.AddWithValue("$id", reminder.IpoEventId);
            command.Parameters.AddWithValue("$version", reminder.EventVersion);
            command.Parameters.AddWithValue("$due", Format(reminder.DueAt));
            command.Parameters.AddWithValue("$level", (int)reminder.Level);
            command.Parameters.AddWithValue("$dedupe", reminder.DedupeKey);
            command.Parameters.AddWithValue("$state", (int)ReminderDeliveryState.Pending);
            command.Parameters.AddWithValue("$now", Format(_timeProvider.GetUtcNow()));
            await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task ReconcileReminderScheduleAsync(
        string eventId,
        int eventVersion,
        IReadOnlyList<ReminderScheduleItem> reminders,
        DateTimeOffset updatedAt,
        CancellationToken cancellationToken = default)
    {
        var current = reminders
            .Where(reminder => string.Equals(reminder.IpoEventId, eventId, StringComparison.OrdinalIgnoreCase)
                && reminder.EventVersion == eventVersion)
            .GroupBy(static reminder => reminder.DedupeKey, StringComparer.Ordinal)
            .Select(static group => group.First())
            .ToArray();

        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);

        await using (var cancel = connection.CreateCommand())
        {
            cancel.Transaction = transaction;
            var keepClause = current.Length == 0
                ? string.Empty
                : $" AND dedupe_key NOT IN ({string.Join(",", current.Select((_, index) => $"$keep{index}"))})";
            cancel.CommandText = $"""
                UPDATE reminder_outbox
                SET delivery_state = $cancelled, lease_until = NULL, updated_at = $updated
                WHERE ipo_event_id = $id AND event_version = $version
                  AND delivery_state IN ($pending, $leased)
                  {keepClause};
                """;
            cancel.Parameters.AddWithValue("$cancelled", (int)ReminderDeliveryState.Cancelled);
            cancel.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
            cancel.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
            cancel.Parameters.AddWithValue("$updated", Format(updatedAt));
            cancel.Parameters.AddWithValue("$id", eventId);
            cancel.Parameters.AddWithValue("$version", eventVersion);
            for (var i = 0; i < current.Length; i++)
            {
                cancel.Parameters.AddWithValue($"$keep{i}", current[i].DedupeKey);
            }

            await cancel.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        foreach (var reminder in current)
        {
            await using var command = connection.CreateCommand();
            command.Transaction = transaction;
            command.CommandText = """
                INSERT INTO reminder_outbox(
                    ipo_event_id, event_version, due_at, reminder_level, dedupe_key,
                    delivery_state, attempt_count, created_at, updated_at)
                VALUES($id, $version, $due, $level, $dedupe, $pending, 0, $updated, $updated)
                ON CONFLICT(dedupe_key) DO UPDATE SET
                    due_at = excluded.due_at,
                    reminder_level = excluded.reminder_level,
                    delivery_state = CASE
                        WHEN reminder_outbox.delivery_state IN ($cancelled, $failed)
                        THEN $pending ELSE reminder_outbox.delivery_state END,
                    lease_until = CASE
                        WHEN reminder_outbox.delivery_state IN ($cancelled, $failed)
                        THEN NULL ELSE reminder_outbox.lease_until END,
                    acknowledged_at = CASE
                        WHEN reminder_outbox.delivery_state IN ($cancelled, $failed)
                        THEN NULL ELSE reminder_outbox.acknowledged_at END,
                    last_error = CASE
                        WHEN reminder_outbox.delivery_state IN ($cancelled, $failed)
                        THEN NULL ELSE reminder_outbox.last_error END,
                    updated_at = excluded.updated_at;
                """;
            command.Parameters.AddWithValue("$id", reminder.IpoEventId);
            command.Parameters.AddWithValue("$version", reminder.EventVersion);
            command.Parameters.AddWithValue("$due", Format(reminder.DueAt));
            command.Parameters.AddWithValue("$level", (int)reminder.Level);
            command.Parameters.AddWithValue("$dedupe", reminder.DedupeKey);
            command.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
            command.Parameters.AddWithValue("$cancelled", (int)ReminderDeliveryState.Cancelled);
            command.Parameters.AddWithValue("$failed", (int)ReminderDeliveryState.Failed);
            command.Parameters.AddWithValue("$updated", Format(updatedAt));
            await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<ReminderDelivery>> ClaimDueRemindersAsync(
        DateTimeOffset now,
        TimeSpan leaseDuration,
        int limit,
        CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        var dueRows = new List<(long Id, string EventId, int EventVersion, DateTimeOffset DueAt, int Level)>();
        await using (var select = connection.CreateCommand())
        {
            select.Transaction = transaction;
            select.CommandText = """
                SELECT o.id, o.ipo_event_id, o.event_version, o.due_at, o.reminder_level
                FROM reminder_outbox o
                JOIN ipo_events e ON e.id = o.ipo_event_id AND e.event_version = o.event_version
                WHERE o.due_at <= $now
                  AND (o.delivery_state = $pending OR (o.delivery_state = $leased AND o.lease_until < $now))
                  AND e.lifecycle_status IN ($scheduled, $active, $review)
                  AND e.issue_status NOT IN ($suspended, $terminated)
                ORDER BY o.due_at, o.id
                LIMIT 1000;
                """;
            select.Parameters.AddWithValue("$now", Format(now));
            select.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
            select.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
            select.Parameters.AddWithValue("$scheduled", (int)IpoLifecycleStatus.Scheduled);
            select.Parameters.AddWithValue("$active", (int)IpoLifecycleStatus.ActiveUnconfirmed);
            select.Parameters.AddWithValue("$review", (int)IpoLifecycleStatus.AcknowledgedNeedsReview);
            select.Parameters.AddWithValue("$suspended", (int)IssueStatus.Suspended);
            select.Parameters.AddWithValue("$terminated", (int)IssueStatus.Terminated);
            await using var reader = await select.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
            while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
            {
                dueRows.Add((
                    reader.GetInt64(0),
                    reader.GetString(1),
                    reader.GetInt32(2),
                    ParseDateTimeOffset(reader.GetString(3)),
                    reader.GetInt32(4)));
            }
        }

        if (dueRows.Count == 0)
        {
            await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
            return [];
        }

        // If the machine was asleep, only deliver the newest currently due reminder for each
        // IPO event. Older due rows are retained in history as collapsed instead of causing a
        // burst of stale windows after resume.
        var newestPerEvent = dueRows
            .GroupBy(static row => (row.EventId, row.EventVersion))
            .Select(static group => group
                .OrderByDescending(static row => row.DueAt)
                .ThenByDescending(static row => row.Level)
                .ThenByDescending(static row => row.Id)
                .First())
            .OrderBy(static row => row.DueAt)
            .ThenBy(static row => row.Id)
            .ToArray();
        var selected = newestPerEvent
            .Take(Math.Clamp(limit, 1, 100))
            .ToArray();
        var ids = selected.Select(static row => row.Id).ToList();
        var selectedIds = ids.ToHashSet();
        var selectedEvents = selected
            .Select(static row => (row.EventId, row.EventVersion))
            .ToHashSet();
        var collapsedIds = dueRows
            .Where(row => selectedEvents.Contains((row.EventId, row.EventVersion)) && !selectedIds.Contains(row.Id))
            .Select(static row => row.Id)
            .ToArray();

        if (collapsedIds.Length > 0)
        {
            var collapsedPlaceholders = string.Join(",", collapsedIds.Select((_, i) => $"$collapsedId{i}"));
            await using var collapse = connection.CreateCommand();
            collapse.Transaction = transaction;
            collapse.CommandText = $"""
                UPDATE reminder_outbox
                SET delivery_state = $collapsed, lease_until = NULL, updated_at = $now
                WHERE id IN ({collapsedPlaceholders});
                """;
            collapse.Parameters.AddWithValue("$collapsed", (int)ReminderDeliveryState.Collapsed);
            collapse.Parameters.AddWithValue("$now", Format(now));
            for (var i = 0; i < collapsedIds.Length; i++)
            {
                collapse.Parameters.AddWithValue($"$collapsedId{i}", collapsedIds[i]);
            }

            await collapse.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        var placeholders = string.Join(",", ids.Select((_, i) => $"$id{i}"));
        await using (var claim = connection.CreateCommand())
        {
            claim.Transaction = transaction;
            claim.CommandText = $"""
                UPDATE reminder_outbox
                SET delivery_state = $leased,
                    lease_until = $leaseUntil,
                    attempt_count = attempt_count + 1,
                    updated_at = $now
                WHERE id IN ({placeholders});
                """;
            claim.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
            claim.Parameters.AddWithValue("$leaseUntil", Format(now.Add(leaseDuration)));
            claim.Parameters.AddWithValue("$now", Format(now));
            for (var i = 0; i < ids.Count; i++)
            {
                claim.Parameters.AddWithValue($"$id{i}", ids[i]);
            }

            await claim.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        var deliveries = new List<ReminderDelivery>();
        await using (var read = connection.CreateCommand())
        {
            read.Transaction = transaction;
            read.CommandText = $"""
                SELECT o.id AS outbox_id, o.due_at, o.reminder_level, o.dedupe_key, o.attempt_count, e.*
                FROM reminder_outbox o
                JOIN ipo_events e ON e.id = o.ipo_event_id
                WHERE o.id IN ({placeholders})
                ORDER BY o.due_at, o.id;
                """;
            for (var i = 0; i < ids.Count; i++)
            {
                read.Parameters.AddWithValue($"$id{i}", ids[i]);
            }

            await using var reader = await read.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
            while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
            {
                deliveries.Add(new ReminderDelivery
                {
                    OutboxId = reader.GetInt64(reader.GetOrdinal("outbox_id")),
                    Event = ReadEvent(reader),
                    DueAt = ParseDateTimeOffset(reader.GetString(reader.GetOrdinal("due_at"))),
                    Level = (ReminderLevel)reader.GetInt32(reader.GetOrdinal("reminder_level")),
                    DedupeKey = reader.GetString(reader.GetOrdinal("dedupe_key")),
                    AttemptCount = reader.GetInt32(reader.GetOrdinal("attempt_count")),
                });
            }
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
        var effectiveDeliveries = new List<ReminderDelivery>(deliveries.Count);
        foreach (var delivery in deliveries)
        {
            var effectiveEvent = await GetEventAsync(delivery.Event.Id, cancellationToken).ConfigureAwait(false);
            effectiveDeliveries.Add(effectiveEvent is null ? delivery : delivery with { Event = effectiveEvent });
        }

        return effectiveDeliveries;
    }

    public Task CompleteReminderAsync(long outboxId, DateTimeOffset shownAt, string deliveryChannel, CancellationToken cancellationToken = default) =>
        FinishReminderAsync(outboxId, shownAt, ReminderDeliveryState.Delivered, deliveryChannel, null, cancellationToken);

    public async Task FailReminderAsync(long outboxId, DateTimeOffset retryAt, string error, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            UPDATE reminder_outbox
            SET delivery_state = $pending, due_at = $retry, lease_until = NULL,
                last_error = $error, updated_at = $now
            WHERE id = $id AND delivery_state = $leased;
            """;
        command.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
        command.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
        command.Parameters.AddWithValue("$retry", Format(retryAt));
        command.Parameters.AddWithValue("$error", error);
        command.Parameters.AddWithValue("$now", Format(_timeProvider.GetUtcNow()));
        command.Parameters.AddWithValue("$id", outboxId);
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<AppSettings> GetSettingsAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT json_value FROM app_settings WHERE id = 1;";
        var value = await command.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false);
        return value is string json ? Deserialize<AppSettings>(json) ?? new AppSettings() : new AppSettings();
    }

    public async Task SaveSettingsAsync(AppSettings settings, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            INSERT INTO app_settings(id, json_value, updated_at)
            VALUES(1, $json, $now)
            ON CONFLICT(id) DO UPDATE SET json_value = excluded.json_value, updated_at = excluded.updated_at;
            """;
        command.Parameters.AddWithValue("$json", JsonSerializer.Serialize(settings, JsonOptions));
        command.Parameters.AddWithValue("$now", Format(_timeProvider.GetUtcNow()));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task TouchHeartbeatAsync(string component, DateTimeOffset timestamp, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            INSERT INTO app_heartbeat(component, heartbeat_at)
            VALUES($component, $at)
            ON CONFLICT(component) DO UPDATE SET heartbeat_at = excluded.heartbeat_at;
            """;
        command.Parameters.AddWithValue("$component", component);
        command.Parameters.AddWithValue("$at", Format(timestamp));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<HealthSummary> GetHealthSummaryAsync(DateOnly date, DateTimeOffset now, CancellationToken cancellationToken = default)
    {
        var settings = await GetSettingsAsync(cancellationToken).ConfigureAwait(false);
        var events = (await GetEventsAsync(date, date, cancellationToken).ConfigureAwait(false))
            .Where(ipoEvent => settings.IsExchangeEnabled(ipoEvent.Exchange))
            .ToArray();
        var sources = await GetSourceHealthAsync(now, events.Length > 0, cancellationToken).ConfigureAwait(false);
        var heartbeats = await GetHeartbeatsAsync(cancellationToken).ConfigureAwait(false);
        DateTimeOffset? scheduler = heartbeats.TryGetValue("scheduler", out var schedulerValue) ? schedulerValue : null;
        DateTimeOffset? delivery = heartbeats.TryGetValue("delivery", out var deliveryValue) ? deliveryValue : null;
        var heartbeatLimit = now.AddMinutes(-3);
        var hasTaskQualityWarning = events.Any(static ipoEvent => ipoEvent.DataQualityStatus is
            DataQualityStatus.DataConflict or DataQualityStatus.Stale or DataQualityStatus.ManualReviewRequired);
        var overall = sources.Count == 0 || sources.All(static x => x.State == HealthState.Failed)
            || scheduler is null || scheduler < heartbeatLimit
            ? HealthState.Failed
            : sources.Any(static x => x.State != HealthState.Healthy)
                || delivery is null || delivery < heartbeatLimit
                || hasTaskQualityWarning
                ? HealthState.Warning
                : HealthState.Healthy;

        return new HealthSummary
        {
            GeneratedAt = now,
            OverallState = overall,
            TodayTaskCount = events.Length,
            PendingConfirmationCount = events.Count(static x => x.LifecycleStatus is IpoLifecycleStatus.Scheduled or IpoLifecycleStatus.ActiveUnconfirmed or IpoLifecycleStatus.AcknowledgedNeedsReview),
            ConflictCount = events.Count(static x => x.DataQualityStatus == DataQualityStatus.DataConflict),
            ManualReviewCount = events.Count(static x => x.DataQualityStatus == DataQualityStatus.ManualReviewRequired),
            SchedulerHeartbeat = scheduler,
            DeliveryHeartbeat = delivery,
            Sources = sources,
        };
    }

    public async Task<bool> TryMarkHealthSummarySentAsync(DateOnly date, DateTimeOffset sentAt, CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "INSERT OR IGNORE INTO health_summary_log(summary_date, sent_at) VALUES($date, $sent);";
        command.Parameters.AddWithValue("$date", Format(date));
        command.Parameters.AddWithValue("$sent", Format(sentAt));
        return await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false) == 1;
    }

    public async Task AddManualOverrideAsync(
        string eventId,
        int eventVersion,
        string fieldName,
        string value,
        string reason,
        string? announcementDocumentId,
        CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        var now = _timeProvider.GetUtcNow();

        await using (var verify = connection.CreateCommand())
        {
            verify.Transaction = transaction;
            verify.CommandText = "SELECT COUNT(*) FROM ipo_events WHERE id = $id AND event_version = $version;";
            verify.Parameters.AddWithValue("$id", eventId);
            verify.Parameters.AddWithValue("$version", eventVersion);
            if (Convert.ToInt32(await verify.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false), CultureInfo.InvariantCulture) != 1)
            {
                throw new InvalidOperationException("申购任务版本已经变化，请刷新详情后重新操作。" );
            }
        }

        await using (var supersede = connection.CreateCommand())
        {
            supersede.Transaction = transaction;
            supersede.CommandText = """
                UPDATE manual_overrides
                SET revoked_at = $now
                WHERE ipo_event_id = $id AND event_version = $version
                  AND field_name = $field AND revoked_at IS NULL;
                """;
            supersede.Parameters.AddWithValue("$now", Format(now));
            supersede.Parameters.AddWithValue("$id", eventId);
            supersede.Parameters.AddWithValue("$version", eventVersion);
            supersede.Parameters.AddWithValue("$field", fieldName);
            await supersede.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using (var insert = connection.CreateCommand())
        {
            insert.Transaction = transaction;
            insert.CommandText = """
                INSERT INTO manual_overrides(
                    ipo_event_id, event_version, field_name, override_value, reason,
                    announcement_document_id, created_at, revoked_at)
                VALUES($id, $version, $field, $value, $reason, $document, $now, NULL);
                """;
            insert.Parameters.AddWithValue("$id", eventId);
            insert.Parameters.AddWithValue("$version", eventVersion);
            insert.Parameters.AddWithValue("$field", fieldName);
            insert.Parameters.AddWithValue("$value", value);
            insert.Parameters.AddWithValue("$reason", reason);
            insert.Parameters.AddWithValue("$document", DbValue(announcementDocumentId));
            insert.Parameters.AddWithValue("$now", Format(now));
            await insert.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await MarkEventForManualReviewAsync(connection, transaction, eventId, eventVersion, now, $"人工覆盖字段 {fieldName}", cancellationToken)
            .ConfigureAwait(false);
        await CancelPendingRemindersAsync(connection, transaction, eventId, eventVersion, now, cancellationToken).ConfigureAwait(false);
        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task RevokeManualOverrideAsync(
        long overrideId,
        DateTimeOffset revokedAt,
        CancellationToken cancellationToken = default)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        string eventId;
        int eventVersion;
        string fieldName;
        await using (var select = connection.CreateCommand())
        {
            select.Transaction = transaction;
            select.CommandText = """
                SELECT ipo_event_id, event_version, field_name
                FROM manual_overrides
                WHERE id = $id AND revoked_at IS NULL;
                """;
            select.Parameters.AddWithValue("$id", overrideId);
            await using var reader = await select.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
            if (!await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
            {
                throw new InvalidOperationException("人工覆盖记录不存在或已经撤销。" );
            }

            eventId = reader.GetString(0);
            eventVersion = reader.GetInt32(1);
            fieldName = reader.GetString(2);
        }

        await using (var revoke = connection.CreateCommand())
        {
            revoke.Transaction = transaction;
            revoke.CommandText = "UPDATE manual_overrides SET revoked_at = $now WHERE id = $id AND revoked_at IS NULL;";
            revoke.Parameters.AddWithValue("$now", Format(revokedAt));
            revoke.Parameters.AddWithValue("$id", overrideId);
            await revoke.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await MarkEventForManualReviewAsync(connection, transaction, eventId, eventVersion, revokedAt, $"撤销人工覆盖字段 {fieldName}", cancellationToken)
            .ConfigureAwait(false);
        await CancelPendingRemindersAsync(connection, transaction, eventId, eventVersion, revokedAt, cancellationToken).ConfigureAwait(false);
        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task FinishReminderAsync(
        long outboxId,
        DateTimeOffset shownAt,
        ReminderDeliveryState state,
        string deliveryChannel,
        string? error,
        CancellationToken cancellationToken)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        await using (var update = connection.CreateCommand())
        {
            update.Transaction = transaction;
            update.CommandText = """
                UPDATE reminder_outbox
                SET delivery_state = $state, delivered_at = $shown, lease_until = NULL,
                    last_error = $error, updated_at = $shown
                WHERE id = $id AND delivery_state = $leased;
                """;
            update.Parameters.AddWithValue("$state", (int)state);
            update.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
            update.Parameters.AddWithValue("$shown", Format(shownAt));
            update.Parameters.AddWithValue("$error", DbValue(error));
            update.Parameters.AddWithValue("$id", outboxId);
            if (await update.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false) == 0)
            {
                await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
                return;
            }
        }

        await using (var log = connection.CreateCommand())
        {
            log.Transaction = transaction;
            log.CommandText = """
                INSERT INTO reminder_log(ipo_event_id, scheduled_at, shown_at, reminder_level, delivery_channel, dedupe_key, result)
                SELECT ipo_event_id, due_at, $shown, reminder_level, $channel, dedupe_key, $result
                FROM reminder_outbox WHERE id = $id;
                """;
            log.Parameters.AddWithValue("$shown", Format(shownAt));
            log.Parameters.AddWithValue("$channel", deliveryChannel);
            log.Parameters.AddWithValue("$result", state.ToString());
            log.Parameters.AddWithValue("$id", outboxId);
            await log.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task<IpoEvent> ApplyManualOverridesAsync(
        SqliteConnection connection,
        SqliteTransaction? transaction,
        IpoEvent ipoEvent,
        CancellationToken cancellationToken)
    {
        var overrides = await ReadManualOverridesAsync(
            connection,
            transaction,
            ipoEvent.Id,
            ipoEvent.EventVersion,
            includeRevoked: false,
            cancellationToken).ConfigureAwait(false);
        if (overrides.Count == 0)
        {
            return ipoEvent;
        }

        var effective = ipoEvent;
        foreach (var entry in overrides
            .GroupBy(static item => item.FieldName, StringComparer.OrdinalIgnoreCase)
            .Select(static group => group.OrderByDescending(item => item.CreatedAt).ThenByDescending(item => item.Id).First()))
        {
            effective = ApplyManualOverride(effective, entry.FieldName, entry.OverrideValue);
        }

        var lifecycle = effective.LifecycleStatus;
        if (effective.Status is IssueStatus.Postponed or IssueStatus.Suspended or IssueStatus.Terminated)
        {
            lifecycle = IpoLifecycleStatus.SuspendedOrCancelled;
        }
        else if (lifecycle is not IpoLifecycleStatus.Acknowledged and not IpoLifecycleStatus.AcknowledgedNeedsReview)
        {
            var today = ChinaTime.Today(_timeProvider);
            lifecycle = effective.ApplyDate switch
            {
                null => IpoLifecycleStatus.Discovered,
                { } date when date > today => IpoLifecycleStatus.Scheduled,
                { } date when date == today => IpoLifecycleStatus.ActiveUnconfirmed,
                _ => IpoLifecycleStatus.ExpiredUnconfirmed,
            };
        }

        return effective with
        {
            LifecycleStatus = lifecycle,
            HasManualOverride = true,
            ManualOverrideFields = overrides
                .Where(static item => item.RevokedAt is null)
                .Select(static item => item.FieldName)
                .Distinct(StringComparer.OrdinalIgnoreCase)
                .Order(StringComparer.OrdinalIgnoreCase)
                .ToArray(),
        };
    }

    private static IpoEvent ApplyManualOverride(IpoEvent ipoEvent, string fieldName, string value) => fieldName switch
    {
        "ApplyCode" => ipoEvent with { ApplyCode = ValueNormalizer.Text(value) ?? ipoEvent.ApplyCode },
        "ApplyDate" => ipoEvent with { ApplyDate = ValueNormalizer.Date(value) ?? ipoEvent.ApplyDate },
        "IssuePrice" => ipoEvent with { IssuePrice = ValueNormalizer.Decimal(value, zeroMeansMissing: true) ?? ipoEvent.IssuePrice },
        "LotSize" => ipoEvent with { LotSize = ValueNormalizer.Integer(value, zeroMeansMissing: true) ?? ipoEvent.LotSize },
        "MaxApplyQuantity" => ipoEvent with { MaxApplyQuantity = ValueNormalizer.Integer(value, zeroMeansMissing: true) ?? ipoEvent.MaxApplyQuantity },
        "IssueStatus" => ipoEvent with { Status = ParseIssueStatus(value, ipoEvent.Status) },
        "OfficialSessions" => ipoEvent with { Sessions = ParseManualSessions(value, ipoEvent) },
        _ => ipoEvent,
    };

    private static IssueStatus ParseIssueStatus(string value, IssueStatus fallback)
    {
        if (Enum.TryParse<IssueStatus>(value, ignoreCase: true, out var parsed))
        {
            return parsed;
        }

        return value.Trim() switch
        {
            "即将发行" or "正常发行" => IssueStatus.Upcoming,
            "申购中" => IssueStatus.Active,
            "延期发行" or "暂缓发行" => IssueStatus.Postponed,
            "中止发行" => IssueStatus.Suspended,
            "终止发行" => IssueStatus.Terminated,
            "发行完成" => IssueStatus.Completed,
            _ => fallback,
        };
    }

    private static IReadOnlyList<SubscriptionSession> ParseManualSessions(string value, IpoEvent ipoEvent)
    {
        var result = new List<SubscriptionSession>();
        var pairs = value.Split([',', '，', ';', '；'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        for (var index = 0; index < pairs.Length; index++)
        {
            var normalized = pairs[index].Replace('—', '-').Replace('–', '-').Replace("至", "-", StringComparison.Ordinal);
            var bounds = normalized.Split('-', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            if (bounds.Length != 2
                || !TimeOnly.TryParseExact(bounds[0], ["H:mm", "HH:mm"], CultureInfo.InvariantCulture, DateTimeStyles.None, out var start)
                || !TimeOnly.TryParseExact(bounds[1], ["H:mm", "HH:mm"], CultureInfo.InvariantCulture, DateTimeStyles.None, out var end))
            {
                continue;
            }

            var existing = ipoEvent.Sessions.FirstOrDefault(session => session.SessionNumber == index + 1);
            result.Add(new SubscriptionSession
            {
                SessionNumber = index + 1,
                OfficialStart = start,
                OfficialEnd = end,
                BrokerAcceptStart = existing?.BrokerAcceptStart,
                SafetyCutoff = existing?.SafetyCutoff,
                FundingMode = existing?.FundingMode ?? (ipoEvent.Exchange == Exchange.Beijing ? FundingMode.FullCash : FundingMode.MarketValue),
                AllocationTimeSensitive = existing?.AllocationTimeSensitive ?? ipoEvent.Exchange == Exchange.Beijing,
                Source = "manual-override",
                SourcePublishedAt = existing?.SourcePublishedAt,
            });
        }

        return result.Count > 0 ? result : ipoEvent.Sessions;
    }

    private static async Task<IReadOnlyList<ManualOverrideEntry>> ReadManualOverridesAsync(
        SqliteConnection connection,
        SqliteTransaction? transaction,
        string eventId,
        int eventVersion,
        bool includeRevoked,
        CancellationToken cancellationToken)
    {
        await using var command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = $"""
            SELECT id, ipo_event_id, event_version, field_name, override_value, reason,
                   announcement_document_id, created_at, revoked_at
            FROM manual_overrides
            WHERE ipo_event_id = $id AND event_version = $version
              {(includeRevoked ? string.Empty : "AND revoked_at IS NULL")}
            ORDER BY created_at DESC, id DESC;
            """;
        command.Parameters.AddWithValue("$id", eventId);
        command.Parameters.AddWithValue("$version", eventVersion);
        var result = new List<ManualOverrideEntry>();
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            result.Add(new ManualOverrideEntry
            {
                Id = reader.GetInt64(reader.GetOrdinal("id")),
                IpoEventId = reader.GetString(reader.GetOrdinal("ipo_event_id")),
                EventVersion = reader.GetInt32(reader.GetOrdinal("event_version")),
                FieldName = reader.GetString(reader.GetOrdinal("field_name")),
                OverrideValue = reader.GetString(reader.GetOrdinal("override_value")),
                Reason = reader.GetString(reader.GetOrdinal("reason")),
                AnnouncementDocumentId = GetNullableString(reader, "announcement_document_id"),
                CreatedAt = ParseDateTimeOffset(reader.GetString(reader.GetOrdinal("created_at"))),
                RevokedAt = GetNullableDateTimeOffset(reader, "revoked_at"),
            });
        }

        return result;
    }

    private static async Task MarkEventForManualReviewAsync(
        SqliteConnection connection,
        SqliteTransaction transaction,
        string eventId,
        int eventVersion,
        DateTimeOffset timestamp,
        string reason,
        CancellationToken cancellationToken)
    {
        await using (var updateEvent = connection.CreateCommand())
        {
            updateEvent.Transaction = transaction;
            updateEvent.CommandText = """
                UPDATE ipo_events
                SET lifecycle_status = CASE WHEN lifecycle_status = $acknowledged THEN $review ELSE lifecycle_status END,
                    updated_at = $now
                WHERE id = $id AND event_version = $version;
                """;
            updateEvent.Parameters.AddWithValue("$acknowledged", (int)IpoLifecycleStatus.Acknowledged);
            updateEvent.Parameters.AddWithValue("$review", (int)IpoLifecycleStatus.AcknowledgedNeedsReview);
            updateEvent.Parameters.AddWithValue("$now", Format(timestamp));
            updateEvent.Parameters.AddWithValue("$id", eventId);
            updateEvent.Parameters.AddWithValue("$version", eventVersion);
            await updateEvent.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await using var updateAck = connection.CreateCommand();
        updateAck.Transaction = transaction;
        updateAck.CommandText = """
            UPDATE acknowledgements
            SET needs_review_at = $now, review_reason = $reason
            WHERE ipo_event_id = $id AND event_version = $version AND revoked_at IS NULL;
            """;
        updateAck.Parameters.AddWithValue("$now", Format(timestamp));
        updateAck.Parameters.AddWithValue("$reason", reason);
        updateAck.Parameters.AddWithValue("$id", eventId);
        updateAck.Parameters.AddWithValue("$version", eventVersion);
        await updateAck.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private static async Task CancelPendingRemindersAsync(
        SqliteConnection connection,
        SqliteTransaction transaction,
        string eventId,
        int eventVersion,
        DateTimeOffset timestamp,
        CancellationToken cancellationToken)
    {
        await using var cancel = connection.CreateCommand();
        cancel.Transaction = transaction;
        cancel.CommandText = """
            UPDATE reminder_outbox
            SET delivery_state = $cancelled, lease_until = NULL, updated_at = $now
            WHERE ipo_event_id = $id AND event_version = $version
              AND delivery_state IN ($pending, $leased);
            """;
        cancel.Parameters.AddWithValue("$cancelled", (int)ReminderDeliveryState.Cancelled);
        cancel.Parameters.AddWithValue("$pending", (int)ReminderDeliveryState.Pending);
        cancel.Parameters.AddWithValue("$leased", (int)ReminderDeliveryState.Leased);
        cancel.Parameters.AddWithValue("$now", Format(timestamp));
        cancel.Parameters.AddWithValue("$id", eventId);
        cancel.Parameters.AddWithValue("$version", eventVersion);
        await cancel.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task<IReadOnlyList<SourceHealth>> GetSourceHealthAsync(DateTimeOffset now, bool activeWindow, CancellationToken cancellationToken)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT * FROM source_health ORDER BY source;";
        var result = new List<SourceHealth>();
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            var lastSuccess = GetNullableDateTimeOffset(reader, "last_success_at");
            var staleAfter = activeWindow ? TimeSpan.FromHours(1) : TimeSpan.FromHours(2);
            var failedAfter = activeWindow ? TimeSpan.FromHours(2) : TimeSpan.FromHours(6);
            var age = lastSuccess is null ? TimeSpan.MaxValue : now - lastSuccess.Value;
            var stored = (HealthState)reader.GetInt32(reader.GetOrdinal("health_state"));
            var state = stored == HealthState.Failed || age > failedAfter
                ? HealthState.Failed
                : age > staleAfter
                    ? HealthState.Warning
                    : HealthState.Healthy;
            result.Add(new SourceHealth
            {
                Source = reader.GetString(reader.GetOrdinal("source")),
                LastAttemptAt = GetNullableDateTimeOffset(reader, "last_attempt_at"),
                LastSuccessAt = lastSuccess,
                LastRecordCount = reader.GetInt32(reader.GetOrdinal("last_record_count")),
                SchemaFingerprint = GetNullableString(reader, "schema_fingerprint"),
                ConsecutiveFailures = reader.GetInt32(reader.GetOrdinal("consecutive_failures")),
                State = state,
                LastError = GetNullableString(reader, "last_error"),
            });
        }

        return result;
    }

    private async Task<Dictionary<string, DateTimeOffset>> GetHeartbeatsAsync(CancellationToken cancellationToken)
    {
        await using var connection = await OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT component, heartbeat_at FROM app_heartbeat;";
        var result = new Dictionary<string, DateTimeOffset>(StringComparer.OrdinalIgnoreCase);
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            result[reader.GetString(0)] = ParseDateTimeOffset(reader.GetString(1));
        }

        return result;
    }

    private async Task<SqliteConnection> OpenAsync(CancellationToken cancellationToken)
    {
        var connection = new SqliteConnection(_options.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 10000;";
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        return connection;
    }

    private static async Task<IpoEvent?> GetEventAsync(SqliteConnection connection, SqliteTransaction? transaction, string id, CancellationToken cancellationToken)
    {
        await using var command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = "SELECT * FROM ipo_events WHERE id = $id LIMIT 1;";
        command.Parameters.AddWithValue("$id", id);
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        return await reader.ReadAsync(cancellationToken).ConfigureAwait(false) ? ReadEvent(reader) : null;
    }

    private static async Task<IReadOnlyList<IpoEvent>> ReadEventsAsync(SqliteCommand command, CancellationToken cancellationToken)
    {
        var result = new List<IpoEvent>();
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            result.Add(ReadEvent(reader));
        }

        return result;
    }

    private static async Task UpsertEventRowAsync(
        SqliteConnection connection,
        SqliteTransaction transaction,
        IpoEvent ipoEvent,
        CancellationToken cancellationToken)
    {
        await using var command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = """
            INSERT INTO ipo_events(
                id, exchange, board, security_code, apply_code, legacy_code, name, apply_date,
                issue_price, lot_size, max_apply_quantity, required_market_value, required_cash,
                ballot_date, payment_date, listing_date, issue_status, lifecycle_status,
                event_version, announcement_url, data_quality_status, data_conflict,
                sessions_json, first_seen_at, updated_at)
            VALUES($id, $exchange, $board, $security, $apply, $legacy, $name, $applyDate,
                   $price, $lot, $maxQty, $marketValue, $cash, $ballot, $payment, $listing,
                   $issueStatus, $lifecycle, $version, $announcement, $quality, $conflict,
                   $sessions, $firstSeen, $updated)
            ON CONFLICT(id) DO UPDATE SET
                exchange = excluded.exchange,
                board = excluded.board,
                security_code = excluded.security_code,
                apply_code = excluded.apply_code,
                legacy_code = excluded.legacy_code,
                name = excluded.name,
                apply_date = excluded.apply_date,
                issue_price = excluded.issue_price,
                lot_size = excluded.lot_size,
                max_apply_quantity = excluded.max_apply_quantity,
                required_market_value = excluded.required_market_value,
                required_cash = excluded.required_cash,
                ballot_date = excluded.ballot_date,
                payment_date = excluded.payment_date,
                listing_date = excluded.listing_date,
                issue_status = excluded.issue_status,
                lifecycle_status = excluded.lifecycle_status,
                event_version = excluded.event_version,
                announcement_url = excluded.announcement_url,
                data_quality_status = excluded.data_quality_status,
                data_conflict = excluded.data_conflict,
                sessions_json = excluded.sessions_json,
                updated_at = excluded.updated_at;
            """;
        command.Parameters.AddWithValue("$id", ipoEvent.Id);
        command.Parameters.AddWithValue("$exchange", (int)ipoEvent.Exchange);
        command.Parameters.AddWithValue("$board", (int)ipoEvent.Board);
        command.Parameters.AddWithValue("$security", ipoEvent.SecurityCode);
        command.Parameters.AddWithValue("$apply", DbValue(ipoEvent.ApplyCode));
        command.Parameters.AddWithValue("$legacy", DbValue(ipoEvent.LegacyCode));
        command.Parameters.AddWithValue("$name", ipoEvent.Name);
        command.Parameters.AddWithValue("$applyDate", ipoEvent.ApplyDate is null ? DBNull.Value : Format(ipoEvent.ApplyDate.Value));
        command.Parameters.AddWithValue("$price", DbValue(ipoEvent.IssuePrice));
        command.Parameters.AddWithValue("$lot", DbValue(ipoEvent.LotSize));
        command.Parameters.AddWithValue("$maxQty", DbValue(ipoEvent.MaxApplyQuantity));
        command.Parameters.AddWithValue("$marketValue", DbValue(ipoEvent.RequiredMarketValue));
        command.Parameters.AddWithValue("$cash", DbValue(ipoEvent.RequiredCash));
        command.Parameters.AddWithValue("$ballot", ipoEvent.BallotDate is null ? DBNull.Value : Format(ipoEvent.BallotDate.Value));
        command.Parameters.AddWithValue("$payment", ipoEvent.PaymentDate is null ? DBNull.Value : Format(ipoEvent.PaymentDate.Value));
        command.Parameters.AddWithValue("$listing", ipoEvent.ListingDate is null ? DBNull.Value : Format(ipoEvent.ListingDate.Value));
        command.Parameters.AddWithValue("$issueStatus", (int)ipoEvent.Status);
        command.Parameters.AddWithValue("$lifecycle", (int)ipoEvent.LifecycleStatus);
        command.Parameters.AddWithValue("$version", ipoEvent.EventVersion);
        command.Parameters.AddWithValue("$announcement", DbValue(ipoEvent.AnnouncementUrl));
        command.Parameters.AddWithValue("$quality", (int)ipoEvent.DataQualityStatus);
        command.Parameters.AddWithValue("$conflict", ipoEvent.DataConflict ? 1 : 0);
        command.Parameters.AddWithValue("$sessions", JsonSerializer.Serialize(ipoEvent.Sessions, JsonOptions));
        command.Parameters.AddWithValue("$firstSeen", Format(ipoEvent.FirstSeenAt));
        command.Parameters.AddWithValue("$updated", Format(ipoEvent.UpdatedAt));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private static async Task ReplaceFieldSourcesAsync(
        SqliteConnection connection,
        SqliteTransaction transaction,
        string eventId,
        IReadOnlyList<SourceFieldValue> sources,
        CancellationToken cancellationToken)
    {
        if (sources.Count == 0)
        {
            return;
        }

        var identities = sources
            .Select(static source => (source.Source, source.FieldName))
            .Distinct()
            .ToArray();
        await using (var delete = connection.CreateCommand())
        {
            delete.Transaction = transaction;
            var predicates = identities.Select((_, index) => $"(source = $source{index} AND field_name = $field{index})");
            delete.CommandText = $"DELETE FROM ipo_field_sources WHERE ipo_event_id = $id AND ({string.Join(" OR ", predicates)});";
            delete.Parameters.AddWithValue("$id", eventId);
            for (var index = 0; index < identities.Length; index++)
            {
                delete.Parameters.AddWithValue($"$source{index}", identities[index].Source);
                delete.Parameters.AddWithValue($"$field{index}", identities[index].FieldName);
            }

            await delete.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        foreach (var source in sources)
        {
            await using var insert = connection.CreateCommand();
            insert.Transaction = transaction;
            insert.CommandText = """
                INSERT INTO ipo_field_sources(
                    ipo_event_id, field_name, normalized_value, raw_value, source, source_published_at,
                    fetched_at, raw_hash, priority)
                VALUES($id, $field, $normalized, $raw, $source, $published, $fetched, $hash, $priority);
                """;
            insert.Parameters.AddWithValue("$id", eventId);
            insert.Parameters.AddWithValue("$field", source.FieldName);
            insert.Parameters.AddWithValue("$normalized", DbValue(source.NormalizedValue));
            insert.Parameters.AddWithValue("$raw", DbValue(source.RawValue));
            insert.Parameters.AddWithValue("$source", source.Source);
            insert.Parameters.AddWithValue("$published", source.SourcePublishedAt is null ? DBNull.Value : Format(source.SourcePublishedAt.Value));
            insert.Parameters.AddWithValue("$fetched", Format(source.FetchedAt));
            insert.Parameters.AddWithValue("$hash", DbValue(source.RawHash));
            insert.Parameters.AddWithValue("$priority", source.Priority);
            await insert.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }
    }

    private static IpoEvent ReadEvent(SqliteDataReader reader) => new()
    {
        Id = reader.GetString(reader.GetOrdinal("id")),
        Exchange = (Exchange)reader.GetInt32(reader.GetOrdinal("exchange")),
        Board = (Board)reader.GetInt32(reader.GetOrdinal("board")),
        SecurityCode = reader.GetString(reader.GetOrdinal("security_code")),
        ApplyCode = GetNullableString(reader, "apply_code"),
        LegacyCode = GetNullableString(reader, "legacy_code"),
        Name = reader.GetString(reader.GetOrdinal("name")),
        ApplyDate = GetNullableDate(reader, "apply_date"),
        IssuePrice = GetNullableDecimal(reader, "issue_price"),
        LotSize = GetNullableInt(reader, "lot_size"),
        MaxApplyQuantity = GetNullableInt(reader, "max_apply_quantity"),
        RequiredMarketValue = GetNullableDecimal(reader, "required_market_value"),
        RequiredCash = GetNullableDecimal(reader, "required_cash"),
        BallotDate = GetNullableDate(reader, "ballot_date"),
        PaymentDate = GetNullableDate(reader, "payment_date"),
        ListingDate = GetNullableDate(reader, "listing_date"),
        Status = (IssueStatus)reader.GetInt32(reader.GetOrdinal("issue_status")),
        LifecycleStatus = (IpoLifecycleStatus)reader.GetInt32(reader.GetOrdinal("lifecycle_status")),
        EventVersion = reader.GetInt32(reader.GetOrdinal("event_version")),
        AnnouncementUrl = GetNullableString(reader, "announcement_url"),
        DataQualityStatus = (DataQualityStatus)reader.GetInt32(reader.GetOrdinal("data_quality_status")),
        DataConflict = reader.GetInt32(reader.GetOrdinal("data_conflict")) != 0,
        Sessions = Deserialize<SubscriptionSession[]>(reader.GetString(reader.GetOrdinal("sessions_json"))) ?? [],
        FirstSeenAt = ParseDateTimeOffset(reader.GetString(reader.GetOrdinal("first_seen_at"))),
        UpdatedAt = ParseDateTimeOffset(reader.GetString(reader.GetOrdinal("updated_at"))),
    };

    private static IReadOnlyList<string> FindChangedFields(IpoEvent previous, IpoEvent current)
    {
        var changed = new List<string>();
        AddIfDifferent(changed, nameof(IpoEvent.ApplyCode), previous.ApplyCode, current.ApplyCode);
        AddIfDifferent(changed, nameof(IpoEvent.ApplyDate), previous.ApplyDate, current.ApplyDate);
        AddIfDifferent(changed, nameof(IpoEvent.IssuePrice), previous.IssuePrice, current.IssuePrice);
        AddIfDifferent(changed, nameof(IpoEvent.Status), previous.Status, current.Status);
        AddIfDifferent(changed, nameof(IpoEvent.LotSize), previous.LotSize, current.LotSize);
        AddIfDifferent(changed, nameof(IpoEvent.MaxApplyQuantity), previous.MaxApplyQuantity, current.MaxApplyQuantity);
        AddIfDifferent(changed, nameof(IpoEvent.AnnouncementUrl), previous.AnnouncementUrl, current.AnnouncementUrl);
        var previousSessions = JsonSerializer.Serialize(previous.Sessions, JsonOptions);
        var currentSessions = JsonSerializer.Serialize(current.Sessions, JsonOptions);
        AddIfDifferent(changed, nameof(IpoEvent.Sessions), previousSessions, currentSessions);
        return changed;
    }

    private static IpoEvent MergeWithExisting(IpoEvent existing, IpoEvent incoming) => incoming with
    {
        Exchange = incoming.Exchange == Exchange.Unknown ? existing.Exchange : incoming.Exchange,
        Board = incoming.Board == Board.Unknown ? existing.Board : incoming.Board,
        SecurityCode = string.IsNullOrWhiteSpace(incoming.SecurityCode) ? existing.SecurityCode : incoming.SecurityCode,
        ApplyCode = ValueNormalizer.Text(incoming.ApplyCode) ?? existing.ApplyCode,
        LegacyCode = ValueNormalizer.Text(incoming.LegacyCode) ?? existing.LegacyCode,
        Name = ValueNormalizer.Text(incoming.Name) ?? existing.Name,
        ApplyDate = incoming.ApplyDate ?? existing.ApplyDate,
        IssuePrice = incoming.IssuePrice ?? existing.IssuePrice,
        LotSize = incoming.LotSize ?? existing.LotSize,
        MaxApplyQuantity = incoming.MaxApplyQuantity ?? existing.MaxApplyQuantity,
        RequiredMarketValue = incoming.RequiredMarketValue ?? existing.RequiredMarketValue,
        RequiredCash = incoming.RequiredCash ?? existing.RequiredCash,
        BallotDate = incoming.BallotDate ?? existing.BallotDate,
        PaymentDate = incoming.PaymentDate ?? existing.PaymentDate,
        ListingDate = incoming.ListingDate ?? existing.ListingDate,
        Status = incoming.Status == IssueStatus.Unknown ? existing.Status : incoming.Status,
        AnnouncementUrl = ValueNormalizer.Text(incoming.AnnouncementUrl) ?? existing.AnnouncementUrl,
        Sessions = incoming.Sessions.Count == 0 ? existing.Sessions : incoming.Sessions,
        FirstSeenAt = existing.FirstSeenAt,
    };

    private static bool IsCriticalField(string name) => name is nameof(IpoEvent.ApplyCode)
        or nameof(IpoEvent.ApplyDate)
        or nameof(IpoEvent.IssuePrice)
        or nameof(IpoEvent.Status)
        or nameof(IpoEvent.LotSize)
        or nameof(IpoEvent.MaxApplyQuantity)
        or nameof(IpoEvent.Sessions);

    private static void AddIfDifferent<T>(ICollection<string> list, string name, T previous, T current)
    {
        if (!EqualityComparer<T>.Default.Equals(previous, current))
        {
            list.Add(name);
        }
    }

    private static string? GetNullableString(SqliteDataReader reader, string name)
    {
        var ordinal = reader.GetOrdinal(name);
        return reader.IsDBNull(ordinal) ? null : reader.GetString(ordinal);
    }

    private static int? GetNullableInt(SqliteDataReader reader, string name)
    {
        var ordinal = reader.GetOrdinal(name);
        return reader.IsDBNull(ordinal) ? null : reader.GetInt32(ordinal);
    }

    private static decimal? GetNullableDecimal(SqliteDataReader reader, string name)
    {
        var ordinal = reader.GetOrdinal(name);
        return reader.IsDBNull(ordinal) ? null : reader.GetDecimal(ordinal);
    }

    private static DateOnly? GetNullableDate(SqliteDataReader reader, string name)
    {
        var ordinal = reader.GetOrdinal(name);
        return reader.IsDBNull(ordinal) ? null : DateOnly.ParseExact(reader.GetString(ordinal), "yyyy-MM-dd", CultureInfo.InvariantCulture);
    }

    private static DateTimeOffset? GetNullableDateTimeOffset(SqliteDataReader reader, string name)
    {
        var ordinal = reader.GetOrdinal(name);
        return reader.IsDBNull(ordinal) ? null : ParseDateTimeOffset(reader.GetString(ordinal));
    }

    private static T? Deserialize<T>(string value) => JsonSerializer.Deserialize<T>(value, JsonOptions);
    private static string Format(DateOnly value) => value.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
    private static string Format(DateTimeOffset value) => value.ToUniversalTime().ToString("O", CultureInfo.InvariantCulture);
    private static DateTimeOffset ParseDateTimeOffset(string value) => DateTimeOffset.Parse(value, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind);
    private static object DbValue<T>(T? value) => value is null ? DBNull.Value : value;

    private const string MigrationSql = """
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;

        CREATE TABLE IF NOT EXISTS schema_migrations(
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ipo_events(
            id TEXT PRIMARY KEY,
            exchange INTEGER NOT NULL,
            board INTEGER NOT NULL,
            security_code TEXT NOT NULL,
            apply_code TEXT NULL,
            legacy_code TEXT NULL,
            name TEXT NOT NULL,
            apply_date TEXT NULL,
            issue_price NUMERIC NULL,
            lot_size INTEGER NULL,
            max_apply_quantity INTEGER NULL,
            required_market_value NUMERIC NULL,
            required_cash NUMERIC NULL,
            ballot_date TEXT NULL,
            payment_date TEXT NULL,
            listing_date TEXT NULL,
            issue_status INTEGER NOT NULL,
            lifecycle_status INTEGER NOT NULL,
            event_version INTEGER NOT NULL,
            announcement_url TEXT NULL,
            data_quality_status INTEGER NOT NULL,
            data_conflict INTEGER NOT NULL DEFAULT 0,
            sessions_json TEXT NOT NULL DEFAULT '[]',
            first_seen_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_ipo_events_apply_date ON ipo_events(apply_date);
        CREATE UNIQUE INDEX IF NOT EXISTS ux_ipo_events_exchange_security ON ipo_events(exchange, security_code);

        CREATE TABLE IF NOT EXISTS ipo_field_sources(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,
            field_name TEXT NOT NULL,
            normalized_value TEXT NULL,
            raw_value TEXT NULL,
            source TEXT NOT NULL,
            source_published_at TEXT NULL,
            fetched_at TEXT NOT NULL,
            raw_hash TEXT NULL,
            priority INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_field_sources_event ON ipo_field_sources(ipo_event_id, field_name);

        CREATE TABLE IF NOT EXISTS acknowledgements(
            ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,
            event_version INTEGER NOT NULL,
            confirmed_at TEXT NOT NULL,
            confirmed_data_hash TEXT NOT NULL,
            needs_review_at TEXT NULL,
            review_reason TEXT NULL,
            reconfirmed_at TEXT NULL,
            revoked_at TEXT NULL,
            PRIMARY KEY(ipo_event_id, event_version)
        );

        CREATE TABLE IF NOT EXISTS reminder_outbox(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,
            event_version INTEGER NOT NULL,
            due_at TEXT NOT NULL,
            reminder_level INTEGER NOT NULL,
            dedupe_key TEXT NOT NULL UNIQUE,
            lease_until TEXT NULL,
            delivery_state INTEGER NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT NULL,
            delivered_at TEXT NULL,
            acknowledged_at TEXT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_outbox_due ON reminder_outbox(delivery_state, due_at);

        CREATE TABLE IF NOT EXISTS reminder_log(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ipo_event_id TEXT NOT NULL,
            scheduled_at TEXT NOT NULL,
            shown_at TEXT NOT NULL,
            reminder_level INTEGER NOT NULL,
            delivery_channel TEXT NOT NULL,
            dedupe_key TEXT NOT NULL,
            result TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS raw_payloads(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            fetched_at TEXT NOT NULL,
            success INTEGER NOT NULL,
            record_count INTEGER NOT NULL,
            raw_hash TEXT NULL,
            schema_fingerprint TEXT NULL,
            payload TEXT NULL,
            error TEXT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_raw_payloads_source_time ON raw_payloads(source, fetched_at DESC);

        CREATE TABLE IF NOT EXISTS sync_runs(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            success INTEGER NOT NULL,
            record_count INTEGER NOT NULL,
            error TEXT NULL
        );

        CREATE TABLE IF NOT EXISTS source_health(
            source TEXT PRIMARY KEY,
            last_attempt_at TEXT NULL,
            last_success_at TEXT NULL,
            last_record_count INTEGER NOT NULL DEFAULT 0,
            schema_fingerprint TEXT NULL,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            health_state INTEGER NOT NULL,
            last_error TEXT NULL
        );

        CREATE TABLE IF NOT EXISTS source_backoff(
            source TEXT PRIMARY KEY,
            failure_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at TEXT NULL,
            last_failure_at TEXT NULL,
            last_success_at TEXT NULL,
            last_error TEXT NULL
        );

        CREATE TABLE IF NOT EXISTS announcement_documents(
            id TEXT PRIMARY KEY,
            ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            announcement_id TEXT NOT NULL,
            announcement_type TEXT NULL,
            title TEXT NOT NULL,
            published_at TEXT NULL,
            source_url TEXT NOT NULL,
            local_path TEXT NOT NULL,
            file_hash TEXT NOT NULL,
            extraction_status INTEGER NOT NULL,
            extracted_text_hash TEXT NULL,
            parser_version TEXT NOT NULL,
            parsed_fields_json TEXT NOT NULL,
            downloaded_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS ux_announcements_provider_id_hash ON announcement_documents(provider, announcement_id, file_hash);

        CREATE TABLE IF NOT EXISTS manual_overrides(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,
            event_version INTEGER NOT NULL,
            field_name TEXT NOT NULL,
            override_value TEXT NOT NULL,
            reason TEXT NOT NULL,
            announcement_document_id TEXT NULL,
            created_at TEXT NOT NULL,
            revoked_at TEXT NULL
        );

        CREATE TABLE IF NOT EXISTS app_settings(
            id INTEGER PRIMARY KEY CHECK(id = 1),
            json_value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_heartbeat(
            component TEXT PRIMARY KEY,
            heartbeat_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS health_summary_log(
            summary_date TEXT PRIMARY KEY,
            sent_at TEXT NOT NULL
        );

        INSERT OR IGNORE INTO schema_migrations(version, applied_at)
        VALUES(1, CURRENT_TIMESTAMP);
        INSERT OR IGNORE INTO schema_migrations(version, applied_at)
        VALUES(2, CURRENT_TIMESTAMP);
        """;
}
