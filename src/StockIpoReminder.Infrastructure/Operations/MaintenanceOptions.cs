namespace StockIpoReminder.Infrastructure.Operations;

public sealed record MaintenanceOptions
{
    public required string DataRoot { get; init; }
    public required string LogDirectory { get; init; }
    public required string BackupDirectory { get; init; }
    public required string DiagnosticDirectory { get; init; }
    public TimeSpan RawPayloadRetention { get; init; } = TimeSpan.FromDays(30);
    public TimeSpan OperationalRetention { get; init; } = TimeSpan.FromDays(90);
    public TimeSpan AuditRetention { get; init; } = TimeSpan.FromDays(730);
    public int BackupRetentionCount { get; init; } = 7;
    public int DeleteBatchSize { get; init; } = 500;
    public TimeSpan InitialDelay { get; init; } = TimeSpan.FromMinutes(2);
    public TimeSpan MaintenanceInterval { get; init; } = TimeSpan.FromHours(24);
}

public sealed record MaintenanceRunResult
{
    public DateTimeOffset StartedAt { get; init; }
    public DateTimeOffset FinishedAt { get; init; }
    public string? BackupPath { get; init; }
    public IReadOnlyDictionary<string, int> DeletedRows { get; init; } = new Dictionary<string, int>();
}

public sealed record DiagnosticExportOptions
{
    public bool IncludeRawPayloads { get; init; }
    public bool IncludeAnnouncementFiles { get; init; }
    public int RecentRecordLimit { get; init; } = 100;
}
