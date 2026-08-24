using System.Globalization;
using Microsoft.Data.Sqlite;
using StockIpoReminder.Infrastructure.Persistence;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed record SourceBackoffOptions
{
    public IReadOnlyList<TimeSpan> FailureDelays { get; init; } =
    [
        TimeSpan.FromMinutes(1),
        TimeSpan.FromMinutes(2),
        TimeSpan.FromMinutes(4),
        TimeSpan.FromMinutes(8),
        TimeSpan.FromMinutes(15),
        TimeSpan.FromMinutes(30),
    ];

    public double JitterRatio { get; init; } = 0.10;
}

public sealed record SourceBackoffDecision
{
    public required string Source { get; init; }
    public bool CanAttempt { get; init; }
    public int FailureCount { get; init; }
    public DateTimeOffset? NextAttemptAt { get; init; }
}

public sealed class SourceBackoffStore
{
    private readonly DatabaseOptions _databaseOptions;
    private readonly SourceBackoffOptions _options;

    public SourceBackoffStore(DatabaseOptions databaseOptions, SourceBackoffOptions options)
    {
        _databaseOptions = databaseOptions;
        _options = options;
    }

    public async Task<SourceBackoffDecision> GetDecisionAsync(
        string source,
        DateTimeOffset now,
        CancellationToken cancellationToken = default)
    {
        await EnsureInitializedAsync(cancellationToken).ConfigureAwait(false);
        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT failure_count, next_attempt_at FROM source_backoff WHERE source = $source;";
        command.Parameters.AddWithValue("$source", source);
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        if (!await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            return new SourceBackoffDecision { Source = source, CanAttempt = true };
        }

        var failures = reader.GetInt32(0);
        DateTimeOffset? nextAttempt = reader.IsDBNull(1)
            ? null
            : DateTimeOffset.Parse(reader.GetString(1), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind);
        return new SourceBackoffDecision
        {
            Source = source,
            FailureCount = failures,
            NextAttemptAt = nextAttempt,
            CanAttempt = nextAttempt is null || now >= nextAttempt,
        };
    }

    public async Task<DateTimeOffset> RecordFailureAsync(
        string source,
        DateTimeOffset now,
        TimeSpan? retryAfter,
        string? error,
        CancellationToken cancellationToken = default)
    {
        await EnsureInitializedAsync(cancellationToken).ConfigureAwait(false);
        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = connection.BeginTransaction(deferred: false);
        var previousFailures = 0;
        await using (var read = connection.CreateCommand())
        {
            read.Transaction = transaction;
            read.CommandText = "SELECT failure_count FROM source_backoff WHERE source = $source;";
            read.Parameters.AddWithValue("$source", source);
            var value = await read.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false);
            if (value is not null and not DBNull)
            {
                previousFailures = Convert.ToInt32(value, CultureInfo.InvariantCulture);
            }
        }

        var failureCount = previousFailures + 1;
        var configured = _options.FailureDelays.Count == 0
            ? TimeSpan.FromMinutes(30)
            : _options.FailureDelays[Math.Min(failureCount - 1, _options.FailureDelays.Count - 1)];
        var delay = retryAfter is { } serverDelay && serverDelay > configured
            ? serverDelay
            : configured;
        var jitterMaximumTicks = (long)Math.Max(0, delay.Ticks * Math.Clamp(_options.JitterRatio, 0, 1));
        var jitterTicks = jitterMaximumTicks == 0
            ? 0
            : Random.Shared.NextInt64(jitterMaximumTicks + 1);
        var nextAttempt = now + delay + TimeSpan.FromTicks(jitterTicks);

        await using (var write = connection.CreateCommand())
        {
            write.Transaction = transaction;
            write.CommandText = """
                INSERT INTO source_backoff(source, failure_count, next_attempt_at, last_failure_at, last_error)
                VALUES($source, $count, $next, $now, $error)
                ON CONFLICT(source) DO UPDATE SET
                    failure_count = excluded.failure_count,
                    next_attempt_at = excluded.next_attempt_at,
                    last_failure_at = excluded.last_failure_at,
                    last_error = excluded.last_error;
                """;
            write.Parameters.AddWithValue("$source", source);
            write.Parameters.AddWithValue("$count", failureCount);
            write.Parameters.AddWithValue("$next", Format(nextAttempt));
            write.Parameters.AddWithValue("$now", Format(now));
            write.Parameters.AddWithValue("$error", error is null ? DBNull.Value : error);
            await write.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        }

        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
        return nextAttempt;
    }

    public async Task RecordSuccessAsync(
        string source,
        DateTimeOffset now,
        CancellationToken cancellationToken = default)
    {
        await EnsureInitializedAsync(cancellationToken).ConfigureAwait(false);
        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            INSERT INTO source_backoff(source, failure_count, next_attempt_at, last_success_at, last_error)
            VALUES($source, 0, NULL, $now, NULL)
            ON CONFLICT(source) DO UPDATE SET
                failure_count = 0,
                next_attempt_at = NULL,
                last_success_at = excluded.last_success_at,
                last_error = NULL;
            """;
        command.Parameters.AddWithValue("$source", source);
        command.Parameters.AddWithValue("$now", Format(now));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task EnsureInitializedAsync(CancellationToken cancellationToken)
    {
        var directory = Path.GetDirectoryName(_databaseOptions.DatabasePath);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            CREATE TABLE IF NOT EXISTS source_backoff(
                source TEXT PRIMARY KEY,
                failure_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT NULL,
                last_failure_at TEXT NULL,
                last_success_at TEXT NULL,
                last_error TEXT NULL
            );
            """;
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private static string Format(DateTimeOffset value) =>
        value.ToUniversalTime().ToString("O", CultureInfo.InvariantCulture);
}
