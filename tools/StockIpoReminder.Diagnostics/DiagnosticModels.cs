using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Diagnostics;

public sealed record DiagnosticReport
{
    public string SchemaVersion { get; init; } = "1";
    public required string RunId { get; init; }
    public required string Mode { get; init; }
    public DateTimeOffset StartedAt { get; init; }
    public DateTimeOffset FinishedAt { get; set; }
    public bool Success { get; set; }
    public required DiagnosticEnvironment Environment { get; init; }
    public required DataIsolationDiagnostic DataIsolation { get; init; }
    public List<DiagnosticCheck> Checks { get; init; } = [];
    public SyncDiagnostic? Synchronization { get; set; }
    public BseSampleDiagnostic? BseSample { get; set; }
    public string? FatalError { get; set; }
}

public sealed record DiagnosticEnvironment
{
    public required string OsDescription { get; init; }
    public required string FrameworkDescription { get; init; }
    public required string ProcessArchitecture { get; init; }
    public required string ShanghaiDate { get; init; }
}

public sealed record DataIsolationDiagnostic
{
    public required string TemporaryDirectoryId { get; init; }
    public bool UsedLocalApplicationData { get; init; }
    public bool KeepRequested { get; init; }
    public bool CleanupAttempted { get; set; }
    public bool CleanupSucceeded { get; set; }
    public string? RetainedDataRoot { get; set; }
    public string? CleanupError { get; set; }
}

public sealed record DiagnosticCheck
{
    public required string Name { get; init; }
    public bool Passed { get; init; }
    public required string Detail { get; init; }
}

public sealed record SyncDiagnostic
{
    public bool ServiceSucceeded { get; init; }
    public int SuccessfulSources { get; init; }
    public int FailedSources { get; init; }
    public int DeferredSources { get; init; }
    public int CandidateCount { get; init; }
    public int EventCount { get; init; }
    public int AnnouncementCount { get; init; }
    public string? Error { get; init; }
    public required string DatabaseIntegrity { get; init; }
    public int PersistedEventCount { get; init; }
    public bool EventsTruncated { get; init; }
    public int ExpectedAnnouncementProviderCount { get; init; }
    public int AttemptedAnnouncementProviderCount { get; init; }
    public int FailedAnnouncementProviderCount { get; init; }
    public int AnnouncementScopeEventCount { get; init; }
    public int EventsWithUsableAnnouncementCount { get; init; }
    public int EventsWithControlledManualReviewCount { get; init; }
    public int UncoveredAnnouncementEventCount { get; init; }
    public int InvalidPdfCount { get; init; }
    public IReadOnlyDictionary<string, int> ExchangeCounts { get; init; } = new Dictionary<string, int>();
    public IReadOnlyList<SourceDiagnostic> Sources { get; init; } = [];
    public IReadOnlyList<EventDiagnostic> Events { get; init; } = [];
}

public sealed record SourceDiagnostic
{
    public required string Source { get; init; }
    public bool AttemptedThisRun { get; init; }
    public bool SucceededThisRun { get; init; }
    public DateTimeOffset? LastAttemptAt { get; init; }
    public DateTimeOffset? LastSuccessAt { get; init; }
    public int RecordCount { get; init; }
    public int ConsecutiveFailures { get; init; }
    public HealthState State { get; init; }
    public string? SchemaFingerprint { get; init; }
    public string? Error { get; init; }
}

public sealed record EventDiagnostic
{
    public required string Id { get; init; }
    public required string Exchange { get; init; }
    public required string Board { get; init; }
    public required string SecurityCode { get; init; }
    public string? ApplyCode { get; init; }
    public string? LegacyCode { get; init; }
    public required string Name { get; init; }
    public string? ApplyDate { get; init; }
    public decimal? IssuePrice { get; init; }
    public int? LotSize { get; init; }
    public int? MaxApplyQuantity { get; init; }
    public required string IssueStatus { get; init; }
    public required string LifecycleStatus { get; init; }
    public required string DataQualityStatus { get; init; }
    public string? AnnouncementUrl { get; init; }
    public IReadOnlyList<SessionDiagnostic> Sessions { get; init; } = [];
    public IReadOnlyList<FieldSourceDiagnostic> FieldSources { get; init; } = [];
    public IReadOnlyList<AnnouncementDiagnostic> Announcements { get; init; } = [];
}

public sealed record SessionDiagnostic
{
    public int SessionNumber { get; init; }
    public required string OfficialStart { get; init; }
    public required string OfficialEnd { get; init; }
    public required string FundingMode { get; init; }
    public bool AllocationTimeSensitive { get; init; }
    public required string Source { get; init; }
}

public sealed record FieldSourceDiagnostic
{
    public required string FieldName { get; init; }
    public string? NormalizedValue { get; init; }
    public required string Source { get; init; }
    public int Priority { get; init; }
    public DateTimeOffset FetchedAt { get; init; }
    public string? RawHash { get; init; }
}

public sealed record AnnouncementDiagnostic
{
    public required string Provider { get; init; }
    public required string AnnouncementId { get; init; }
    public required string Title { get; init; }
    public required string Url { get; init; }
    public DateTimeOffset? PublishedAt { get; init; }
    public required string FileName { get; init; }
    public long FileLength { get; init; }
    public required string FileHash { get; init; }
    public required string ExtractionStatus { get; init; }
    public required string ParserVersion { get; init; }
    public IReadOnlyList<ParsedFieldDiagnostic> Fields { get; init; } = [];
}

public sealed record ParsedFieldDiagnostic
{
    public required string Name { get; init; }
    public required string Value { get; init; }
    public decimal Confidence { get; init; }
    public string? EvidenceSummary { get; init; }
    public int? CharacterOffset { get; init; }
}

public sealed record BseSampleDiagnostic
{
    public bool Success { get; init; }
    public int ReferenceCount { get; init; }
    public string? SelectedTitle { get; init; }
    public string? SelectedUrl { get; init; }
    public string? FileHash { get; init; }
    public long? FileLength { get; init; }
    public string? ExtractionStatus { get; init; }
    public string? ParserVersion { get; init; }
    public IReadOnlyList<ParsedFieldDiagnostic> Fields { get; init; } = [];
    public string? Error { get; init; }
}
