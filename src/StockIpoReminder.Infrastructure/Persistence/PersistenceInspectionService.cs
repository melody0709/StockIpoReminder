using System.Globalization;
using Microsoft.Data.Sqlite;
using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Infrastructure.Persistence;

public sealed class PersistenceInspectionService
{
    private readonly DatabaseOptions _options;

    public PersistenceInspectionService(DatabaseOptions options)
    {
        _options = options;
    }

    public async Task<ReminderPersistenceSnapshot> InspectReminderAsync(
        string dedupeKey,
        string acknowledgedEventId,
        CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(dedupeKey);
        ArgumentException.ThrowIfNullOrWhiteSpace(acknowledgedEventId);

        await using var connection = new SqliteConnection(_options.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);

        var outbox = new List<ReminderOutboxInspection>();
        await using (var command = connection.CreateCommand())
        {
            command.CommandText = """
                SELECT id, delivery_state, attempt_count, lease_until
                FROM reminder_outbox
                WHERE dedupe_key = $dedupe
                ORDER BY id;
                """;
            command.Parameters.AddWithValue("$dedupe", dedupeKey);
            await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
            while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
            {
                outbox.Add(new ReminderOutboxInspection
                {
                    OutboxId = reader.GetInt64(0),
                    State = (ReminderDeliveryState)reader.GetInt32(1),
                    AttemptCount = reader.GetInt32(2),
                    LeaseUntil = reader.IsDBNull(3)
                        ? null
                        : DateTimeOffset.Parse(reader.GetString(3), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind),
                });
            }
        }

        var reminderLogCount = await CountAsync(
            connection,
            "SELECT COUNT(*) FROM reminder_log WHERE dedupe_key = $value;",
            dedupeKey,
            cancellationToken).ConfigureAwait(false);
        var activeAcknowledgementCount = await CountAsync(
            connection,
            "SELECT COUNT(*) FROM acknowledgements WHERE ipo_event_id = $value AND revoked_at IS NULL;",
            acknowledgedEventId,
            cancellationToken).ConfigureAwait(false);

        await using var integrity = connection.CreateCommand();
        integrity.CommandText = "PRAGMA integrity_check;";
        var integrityResult = Convert.ToString(
            await integrity.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false),
            CultureInfo.InvariantCulture) ?? string.Empty;

        return new ReminderPersistenceSnapshot
        {
            Outbox = outbox,
            ReminderLogCount = reminderLogCount,
            ActiveAcknowledgementCount = activeAcknowledgementCount,
            IntegrityResult = integrityResult,
        };
    }

    private static async Task<long> CountAsync(
        SqliteConnection connection,
        string sql,
        string value,
        CancellationToken cancellationToken)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        command.Parameters.AddWithValue("$value", value);
        return Convert.ToInt64(
            await command.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false),
            CultureInfo.InvariantCulture);
    }
}

public sealed record ReminderPersistenceSnapshot
{
    public IReadOnlyList<ReminderOutboxInspection> Outbox { get; init; } = [];

    public long ReminderLogCount { get; init; }

    public long ActiveAcknowledgementCount { get; init; }

    public string IntegrityResult { get; init; } = string.Empty;
}

public sealed record ReminderOutboxInspection
{
    public long OutboxId { get; init; }

    public ReminderDeliveryState State { get; init; }

    public int AttemptCount { get; init; }

    public DateTimeOffset? LeaseUntil { get; init; }
}
