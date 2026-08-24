using System.Globalization;
using Microsoft.Data.Sqlite;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Persistence;

namespace StockIpoReminder.Infrastructure.Operations;

public sealed class OperationalMaintenanceService : BackgroundService
{
    private readonly IIpoRepository _repository;
    private readonly DatabaseOptions _databaseOptions;
    private readonly MaintenanceOptions _options;
    private readonly TimeProvider _timeProvider;
    private readonly ILogger<OperationalMaintenanceService> _logger;

    public OperationalMaintenanceService(
        IIpoRepository repository,
        DatabaseOptions databaseOptions,
        MaintenanceOptions options,
        TimeProvider timeProvider,
        ILogger<OperationalMaintenanceService> logger)
    {
        _repository = repository;
        _databaseOptions = databaseOptions;
        _options = options;
        _timeProvider = timeProvider;
        _logger = logger;
    }

    public async Task<MaintenanceRunResult> RunOnceAsync(CancellationToken cancellationToken = default)
    {
        var startedAt = _timeProvider.GetUtcNow();
        await _repository.InitializeAsync(cancellationToken).ConfigureAwait(false);
        var deleted = await CleanupAsync(startedAt, cancellationToken).ConfigureAwait(false);
        await CheckpointAsync(cancellationToken).ConfigureAwait(false);
        var backupPath = await CreateBackupAsync(startedAt, cancellationToken).ConfigureAwait(false);
        return new MaintenanceRunResult
        {
            StartedAt = startedAt,
            FinishedAt = _timeProvider.GetUtcNow(),
            BackupPath = backupPath,
            DeletedRows = deleted,
        };
    }

    public async Task<string> CreateBackupAsync(DateTimeOffset timestamp, CancellationToken cancellationToken = default)
    {
        Directory.CreateDirectory(_options.BackupDirectory);
        var finalPath = ResolveBackupPath(timestamp);
        var temporaryPath = finalPath + $".tmp-{Guid.NewGuid():N}";
        try
        {
            cancellationToken.ThrowIfCancellationRequested();
            await Task.Run(() =>
            {
                using var source = new SqliteConnection(_databaseOptions.ConnectionString);
                using var destination = new SqliteConnection(new SqliteConnectionStringBuilder
                {
                    DataSource = temporaryPath,
                    Mode = SqliteOpenMode.ReadWriteCreate,
                    Cache = SqliteCacheMode.Private,
                    Pooling = false,
                }.ToString());
                source.Open();
                destination.Open();
                source.BackupDatabase(destination);
            }, cancellationToken).ConfigureAwait(false);

            await VerifyBackupAsync(temporaryPath, cancellationToken).ConfigureAwait(false);
            File.Move(temporaryPath, finalPath);
            await TrimBackupsAsync(cancellationToken).ConfigureAwait(false);
            return finalPath;
        }
        finally
        {
            if (File.Exists(temporaryPath))
            {
                File.Delete(temporaryPath);
            }
        }
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        if (_options.InitialDelay > TimeSpan.Zero)
        {
            await Task.Delay(_options.InitialDelay, stoppingToken).ConfigureAwait(false);
        }

        while (!stoppingToken.IsCancellationRequested)
        {
            try
            {
                var result = await RunOnceAsync(stoppingToken).ConfigureAwait(false);
                _logger.LogInformation(
                    "运维完成，备份 {BackupPath}，清理 {DeletedRows} 条记录",
                    result.BackupPath,
                    result.DeletedRows.Values.Sum());
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                break;
            }
            catch (Exception ex)
            {
                _logger.LogError(ex, "后台数据清理或备份失败，提醒服务继续运行");
            }

            await Task.Delay(_options.MaintenanceInterval, stoppingToken).ConfigureAwait(false);
        }
    }

    private async Task<IReadOnlyDictionary<string, int>> CleanupAsync(
        DateTimeOffset now,
        CancellationToken cancellationToken)
    {
        var deleted = new Dictionary<string, int>(StringComparer.Ordinal);
        deleted["raw_payloads"] = await DeleteInBatchesAsync(
            """
            DELETE FROM raw_payloads
            WHERE rowid IN (
                SELECT payload.rowid
                FROM raw_payloads payload
                WHERE payload.fetched_at < $cutoff
                  AND NOT (
                    payload.raw_hash IS NOT NULL
                    AND EXISTS (
                        SELECT 1
                        FROM ipo_field_sources field
                        JOIN ipo_events event ON event.id = field.ipo_event_id
                        WHERE field.raw_hash = payload.raw_hash
                          AND (
                            event.lifecycle_status IN ($discovered, $scheduled, $active, $needsReview)
                            OR event.data_conflict = 1
                            OR event.data_quality_status IN ($conflict, $manualReview)
                          )
                    )
                  )
                LIMIT $batch
            );
            """,
            now - _options.RawPayloadRetention,
            cancellationToken).ConfigureAwait(false);

        deleted["sync_runs"] = await DeleteInBatchesAsync(
            "DELETE FROM sync_runs WHERE rowid IN (SELECT rowid FROM sync_runs WHERE finished_at < $cutoff LIMIT $batch);",
            now - _options.OperationalRetention,
            cancellationToken).ConfigureAwait(false);
        deleted["health_summary_log"] = await DeleteInBatchesAsync(
            "DELETE FROM health_summary_log WHERE rowid IN (SELECT rowid FROM health_summary_log WHERE sent_at < $cutoff LIMIT $batch);",
            now - _options.OperationalRetention,
            cancellationToken).ConfigureAwait(false);

        deleted["reminder_log"] = await DeleteInBatchesAsync(
            $"""
            DELETE FROM reminder_log
            WHERE rowid IN (
                SELECT log.rowid
                FROM reminder_log log
                WHERE log.shown_at < $cutoff
                  AND {NotProtectedEventSql("log.ipo_event_id")}
                LIMIT $batch
            );
            """,
            now - _options.OperationalRetention,
            cancellationToken).ConfigureAwait(false);
        deleted["reminder_outbox"] = await DeleteInBatchesAsync(
            $"""
            DELETE FROM reminder_outbox
            WHERE rowid IN (
                SELECT outbox.rowid
                FROM reminder_outbox outbox
                WHERE outbox.updated_at < $cutoff
                  AND outbox.delivery_state IN ($delivered, $collapsed, $cancelled, $failed)
                  AND {NotProtectedEventSql("outbox.ipo_event_id")}
                LIMIT $batch
            );
            """,
            now - _options.OperationalRetention,
            cancellationToken).ConfigureAwait(false);

        deleted["acknowledgements"] = await DeleteInBatchesAsync(
            $"""
            DELETE FROM acknowledgements
            WHERE rowid IN (
                SELECT acknowledgement.rowid
                FROM acknowledgements acknowledgement
                WHERE acknowledgement.confirmed_at < $cutoff
                  AND {NotProtectedEventSql("acknowledgement.ipo_event_id")}
                LIMIT $batch
            );
            """,
            now - _options.AuditRetention,
            cancellationToken).ConfigureAwait(false);
        deleted["manual_overrides"] = await DeleteInBatchesAsync(
            $"""
            DELETE FROM manual_overrides
            WHERE rowid IN (
                SELECT override.rowid
                FROM manual_overrides override
                WHERE override.revoked_at IS NOT NULL
                  AND override.revoked_at < $cutoff
                  AND {NotProtectedEventSql("override.ipo_event_id")}
                LIMIT $batch
            );
            """,
            now - _options.AuditRetention,
            cancellationToken).ConfigureAwait(false);
        deleted["announcement_documents"] = await DeleteAnnouncementsAsync(
            now - _options.AuditRetention,
            cancellationToken).ConfigureAwait(false);
        return deleted;
    }

    private async Task<int> DeleteInBatchesAsync(
        string commandText,
        DateTimeOffset cutoff,
        CancellationToken cancellationToken)
    {
        var total = 0;
        while (true)
        {
            await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
            await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
            await using var transaction = connection.BeginTransaction(deferred: false);
            await using var command = connection.CreateCommand();
            command.Transaction = transaction;
            command.CommandText = commandText;
            AddCommonParameters(command, cutoff);
            var count = await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
            await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
            total += count;
            if (count < _options.DeleteBatchSize)
            {
                return total;
            }
        }
    }

    private async Task<int> DeleteAnnouncementsAsync(DateTimeOffset cutoff, CancellationToken cancellationToken)
    {
        var total = 0;
        while (true)
        {
            var deletedPaths = new List<string>();
            var count = 0;
            await using (var connection = new SqliteConnection(_databaseOptions.ConnectionString))
            {
                await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
                await using var transaction = connection.BeginTransaction(deferred: false);
                var candidates = new List<(string Id, string Path)>();
                await using (var query = connection.CreateCommand())
                {
                    query.Transaction = transaction;
                    query.CommandText = $"""
                        SELECT document.id, document.local_path
                        FROM announcement_documents document
                        WHERE document.downloaded_at < $cutoff
                          AND {NotProtectedEventSql("document.ipo_event_id")}
                        LIMIT $batch;
                        """;
                    AddCommonParameters(query, cutoff);
                    await using var reader = await query.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
                    while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
                    {
                        candidates.Add((reader.GetString(0), reader.GetString(1)));
                    }
                }

                foreach (var candidate in candidates)
                {
                    await using var delete = connection.CreateCommand();
                    delete.Transaction = transaction;
                    delete.CommandText = "DELETE FROM announcement_documents WHERE id = $id;";
                    delete.Parameters.AddWithValue("$id", candidate.Id);
                    count += await delete.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
                    deletedPaths.Add(candidate.Path);
                }

                await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
            }

            total += count;
            foreach (var path in deletedPaths.Distinct(StringComparer.OrdinalIgnoreCase))
            {
                await DeleteUnreferencedAnnouncementFileAsync(path, cancellationToken).ConfigureAwait(false);
            }

            if (count < _options.DeleteBatchSize)
            {
                return total;
            }
        }
    }

    private async Task DeleteUnreferencedAnnouncementFileAsync(string path, CancellationToken cancellationToken)
    {
        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT COUNT(*) FROM announcement_documents WHERE local_path = $path;";
        command.Parameters.AddWithValue("$path", path);
        var references = Convert.ToInt64(await command.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false), CultureInfo.InvariantCulture);
        if (references != 0 || !IsWithinDirectory(path, _options.DataRoot) || !File.Exists(path))
        {
            return;
        }

        try
        {
            File.Delete(path);
        }
        catch (IOException ex)
        {
            _logger.LogWarning(ex, "无法删除已过期公告文件 {AnnouncementPath}", path);
        }
        catch (UnauthorizedAccessException ex)
        {
            _logger.LogWarning(ex, "没有权限删除已过期公告文件 {AnnouncementPath}", path);
        }
    }

    private async Task CheckpointAsync(CancellationToken cancellationToken)
    {
        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "PRAGMA wal_checkpoint(PASSIVE);";
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task VerifyBackupAsync(string path, CancellationToken cancellationToken)
    {
        await using var connection = new SqliteConnection(new SqliteConnectionStringBuilder
        {
            DataSource = path,
            Mode = SqliteOpenMode.ReadOnly,
            Cache = SqliteCacheMode.Private,
            Pooling = false,
        }.ToString());
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "PRAGMA integrity_check;";
        var result = Convert.ToString(await command.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false), CultureInfo.InvariantCulture);
        if (!string.Equals(result, "ok", StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidDataException($"SQLite 备份完整性检查失败：{result}");
        }
    }

    private Task TrimBackupsAsync(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var keep = Math.Max(1, _options.BackupRetentionCount);
        var backups = Directory.EnumerateFiles(_options.BackupDirectory, "stock-ipo-reminder-*.db", SearchOption.TopDirectoryOnly)
            .Select(static path => new FileInfo(path))
            .OrderByDescending(static file => file.LastWriteTimeUtc)
            .ThenByDescending(static file => file.Name, StringComparer.Ordinal)
            .Skip(keep)
            .ToArray();
        foreach (var backup in backups)
        {
            cancellationToken.ThrowIfCancellationRequested();
            backup.Delete();
        }

        return Task.CompletedTask;
    }

    private string ResolveBackupPath(DateTimeOffset timestamp)
    {
        var baseName = $"stock-ipo-reminder-{timestamp.ToUniversalTime():yyyyMMdd-HHmmssfff}";
        for (var index = 0; ; index++)
        {
            var suffix = index == 0 ? string.Empty : $"-{index}";
            var path = Path.Combine(_options.BackupDirectory, $"{baseName}{suffix}.db");
            if (!File.Exists(path))
            {
                return path;
            }
        }
    }

    private void AddCommonParameters(SqliteCommand command, DateTimeOffset cutoff)
    {
        command.Parameters.AddWithValue("$cutoff", cutoff.ToUniversalTime().ToString("O", CultureInfo.InvariantCulture));
        command.Parameters.AddWithValue("$batch", Math.Clamp(_options.DeleteBatchSize, 10, 10_000));
        command.Parameters.AddWithValue("$discovered", (int)IpoLifecycleStatus.Discovered);
        command.Parameters.AddWithValue("$scheduled", (int)IpoLifecycleStatus.Scheduled);
        command.Parameters.AddWithValue("$active", (int)IpoLifecycleStatus.ActiveUnconfirmed);
        command.Parameters.AddWithValue("$needsReview", (int)IpoLifecycleStatus.AcknowledgedNeedsReview);
        command.Parameters.AddWithValue("$conflict", (int)DataQualityStatus.DataConflict);
        command.Parameters.AddWithValue("$manualReview", (int)DataQualityStatus.ManualReviewRequired);
        command.Parameters.AddWithValue("$delivered", (int)ReminderDeliveryState.Delivered);
        command.Parameters.AddWithValue("$collapsed", (int)ReminderDeliveryState.Collapsed);
        command.Parameters.AddWithValue("$cancelled", (int)ReminderDeliveryState.Cancelled);
        command.Parameters.AddWithValue("$failed", (int)ReminderDeliveryState.Failed);
    }

    private static string NotProtectedEventSql(string eventIdExpression) => $"""
        NOT EXISTS (
            SELECT 1
            FROM ipo_events protected_event
            WHERE protected_event.id = {eventIdExpression}
              AND (
                protected_event.lifecycle_status IN ($discovered, $scheduled, $active, $needsReview)
                OR protected_event.data_conflict = 1
                OR protected_event.data_quality_status IN ($conflict, $manualReview)
              )
        )
        """;

    private static bool IsWithinDirectory(string path, string directory)
    {
        var fullPath = Path.GetFullPath(path);
        var fullDirectory = Path.GetFullPath(directory).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)
            + Path.DirectorySeparatorChar;
        return fullPath.StartsWith(fullDirectory, StringComparison.OrdinalIgnoreCase);
    }
}
