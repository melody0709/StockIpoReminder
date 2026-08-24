namespace StockIpoReminder.Core.Domain;

public sealed record CollectorResult
{
    public required string Source { get; init; }
    public bool Success { get; init; }
    public DateTimeOffset StartedAt { get; init; }
    public DateTimeOffset FinishedAt { get; init; }
    public IReadOnlyList<IpoCandidate> Candidates { get; init; } = [];
    public string? RawPayload { get; init; }
    public string? RawHash { get; init; }
    public string? SchemaFingerprint { get; init; }
    public string? Error { get; init; }
    public int RecordCount { get; init; }
    public TimeSpan? RetryAfter { get; init; }
    public DateTimeOffset? DeferredUntil { get; init; }
    public bool IsDeferred => DeferredUntil is not null;

    public static CollectorResult Failed(string source, DateTimeOffset startedAt, DateTimeOffset finishedAt, Exception error) =>
        new()
        {
            Source = source,
            Success = false,
            StartedAt = startedAt,
            FinishedAt = finishedAt,
            Error = error.ToString(),
            RetryAfter = (error as Abstractions.IRetryAfterError)?.RetryAfter,
        };

    public static CollectorResult Deferred(string source, DateTimeOffset now, DateTimeOffset until) =>
        new()
        {
            Source = source,
            Success = false,
            StartedAt = now,
            FinishedAt = now,
            DeferredUntil = until,
            Error = $"来源处于退避状态，将在 {until:O} 后重试。",
        };
}

public sealed record AnnouncementReference
{
    public required string Provider { get; init; }
    public required string AnnouncementId { get; init; }
    public required string Title { get; init; }
    public required Uri Url { get; init; }
    public DateTimeOffset? PublishedAt { get; init; }
    public string? AnnouncementType { get; init; }
}

public sealed record ParsedAnnouncementField
{
    public required string Name { get; init; }
    public required string Value { get; init; }
    public required decimal Confidence { get; init; }
    public string? Evidence { get; init; }
    public int? CharacterOffset { get; init; }
}

public sealed record AnnouncementParseResult
{
    public ExtractionStatus Status { get; init; }
    public string? ExtractedText { get; init; }
    public IReadOnlyList<ParsedAnnouncementField> Fields { get; init; } = [];
    public string? Error { get; init; }
}

public sealed record AnnouncementDocument
{
    public required string Id { get; init; }
    public required string IpoEventId { get; init; }
    public required AnnouncementReference Reference { get; init; }
    public required string LocalPath { get; init; }
    public required string FileHash { get; init; }
    public string? ExtractedTextHash { get; init; }
    public ExtractionStatus ExtractionStatus { get; init; }
    public string ParserVersion { get; init; } = "1";
    public IReadOnlyList<ParsedAnnouncementField> ParsedFields { get; init; } = [];
    public DateTimeOffset DownloadedAt { get; init; }
}

public sealed record SourceHealth
{
    public required string Source { get; init; }
    public DateTimeOffset? LastAttemptAt { get; init; }
    public DateTimeOffset? LastSuccessAt { get; init; }
    public int LastRecordCount { get; init; }
    public string? SchemaFingerprint { get; init; }
    public int ConsecutiveFailures { get; init; }
    public HealthState State { get; init; }
    public string? LastError { get; init; }
}
