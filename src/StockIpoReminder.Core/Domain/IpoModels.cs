using System.Collections.ObjectModel;

namespace StockIpoReminder.Core.Domain;

public sealed record SubscriptionSession
{
    public int SessionNumber { get; init; }
    public TimeOnly OfficialStart { get; init; }
    public TimeOnly OfficialEnd { get; init; }
    public TimeOnly? BrokerAcceptStart { get; init; }
    public TimeOnly? SafetyCutoff { get; init; }
    public FundingMode FundingMode { get; init; }
    public bool AllocationTimeSensitive { get; init; }
    public string Source { get; init; } = "default";
    public DateTimeOffset? SourcePublishedAt { get; init; }
}

public sealed record IpoEvent
{
    public required string Id { get; init; }
    public Exchange Exchange { get; init; }
    public Board Board { get; init; }
    public required string SecurityCode { get; init; }
    public string? ApplyCode { get; init; }
    public string? LegacyCode { get; init; }
    public required string Name { get; init; }
    public DateOnly? ApplyDate { get; init; }
    public decimal? IssuePrice { get; init; }
    public int? LotSize { get; init; }
    public int? MaxApplyQuantity { get; init; }
    public decimal? RequiredMarketValue { get; init; }
    public decimal? RequiredCash { get; init; }
    public DateOnly? BallotDate { get; init; }
    public DateOnly? PaymentDate { get; init; }
    public DateOnly? ListingDate { get; init; }
    public IssueStatus Status { get; init; } = IssueStatus.Unknown;
    public IpoLifecycleStatus LifecycleStatus { get; init; } = IpoLifecycleStatus.Discovered;
    public int EventVersion { get; init; } = 1;
    public string? AnnouncementUrl { get; init; }
    public DataQualityStatus DataQualityStatus { get; init; } = DataQualityStatus.SingleSource;
    public bool DataConflict { get; init; }
    public bool HasManualOverride { get; init; }
    public IReadOnlyList<string> ManualOverrideFields { get; init; } = [];
    public DateTimeOffset FirstSeenAt { get; init; }
    public DateTimeOffset UpdatedAt { get; init; }
    public IReadOnlyList<SubscriptionSession> Sessions { get; init; } = [];

    public bool IsTerminal => Status is IssueStatus.Terminated or IssueStatus.Suspended
        || LifecycleStatus is IpoLifecycleStatus.SuspendedOrCancelled
            or IpoLifecycleStatus.ExpiredUnconfirmed
            or IpoLifecycleStatus.Superseded;

    public string DisplayCode => string.IsNullOrWhiteSpace(ApplyCode) ? SecurityCode : ApplyCode;
}

public sealed record SourceFieldValue
{
    public required string FieldName { get; init; }
    public string? RawValue { get; init; }
    public string? NormalizedValue { get; init; }
    public required string Source { get; init; }
    public int Priority { get; init; }
    public DateTimeOffset? SourcePublishedAt { get; init; }
    public DateTimeOffset FetchedAt { get; init; }
    public string? RawHash { get; init; }
}

public sealed record IpoCandidate
{
    public required string Source { get; init; }
    public int SourcePriority { get; init; }
    public DateTimeOffset FetchedAt { get; init; }
    public DateTimeOffset? SourcePublishedAt { get; init; }
    public Exchange Exchange { get; init; }
    public Board Board { get; init; }
    public string? SecurityCode { get; init; }
    public string? ApplyCode { get; init; }
    public string? LegacyCode { get; init; }
    public string? Name { get; init; }
    public DateOnly? ApplyDate { get; init; }
    public decimal? IssuePrice { get; init; }
    public int? LotSize { get; init; }
    public int? MaxApplyQuantity { get; init; }
    public decimal? RequiredMarketValue { get; init; }
    public decimal? RequiredCash { get; init; }
    public DateOnly? BallotDate { get; init; }
    public DateOnly? PaymentDate { get; init; }
    public DateOnly? ListingDate { get; init; }
    public IssueStatus Status { get; init; }
    public string? AnnouncementUrl { get; init; }
    public IReadOnlyList<SubscriptionSession> Sessions { get; init; } = [];
    public IReadOnlyList<SourceFieldValue> Fields { get; init; } = [];
    public bool IsAnnouncementDerived { get; init; }

    public string? StableIdentity => !string.IsNullOrWhiteSpace(SecurityCode)
        ? IpoEventIdentity.Create(Exchange, SecurityCode)
        : !string.IsNullOrWhiteSpace(ApplyCode)
            ? $"{Exchange.ToString().ToLowerInvariant()}:apply:{ApplyCode}"
            : null;
}

public sealed record ReconciledIpoEvent
{
    public required IpoEvent Event { get; init; }
    public IReadOnlyList<SourceFieldValue> FieldSources { get; init; } = [];
    public IReadOnlyList<string> ConflictFields { get; init; } = [];
}

public sealed record UpsertEventResult
{
    public required IpoEvent Event { get; init; }
    public bool Created { get; init; }
    public bool EventVersionChanged { get; init; }
    public bool CriticalFieldsChanged { get; init; }
    public IReadOnlyList<string> ChangedFields { get; init; } = [];
}

public static class IpoEventIdentity
{
    public static string Create(Exchange exchange, string securityCode) =>
        $"{exchange.ToString().ToLowerInvariant()}:{securityCode.Trim()}";
}

public sealed record ManualOverrideEntry
{
    public long Id { get; init; }
    public required string IpoEventId { get; init; }
    public int EventVersion { get; init; }
    public required string FieldName { get; init; }
    public required string OverrideValue { get; init; }
    public required string Reason { get; init; }
    public string? AnnouncementDocumentId { get; init; }
    public DateTimeOffset CreatedAt { get; init; }
    public DateTimeOffset? RevokedAt { get; init; }
}

public sealed record IpoEventDetails
{
    public required IpoEvent Event { get; init; }
    public IReadOnlyList<SourceFieldValue> FieldSources { get; init; } = [];
    public IReadOnlyList<AnnouncementDocument> Announcements { get; init; } = [];
    public IReadOnlyList<ManualOverrideEntry> ManualOverrides { get; init; } = [];
}
