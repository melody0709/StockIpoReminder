using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Core.Abstractions;

public interface IRetryAfterError
{
    TimeSpan? RetryAfter { get; }
}

public interface IIpoCollector
{
    string SourceName { get; }
    int Priority { get; }
    Task<CollectorResult> CollectAsync(CancellationToken cancellationToken);
}

public interface IAnnouncementProvider
{
    string ProviderName { get; }
    bool Supports(Exchange exchange);
    Task<IReadOnlyList<AnnouncementReference>> SearchAsync(
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken);
}

public interface IAnnouncementProcessor
{
    Task<AnnouncementDocument> DownloadAndParseAsync(
        IpoEvent ipoEvent,
        AnnouncementReference announcement,
        CancellationToken cancellationToken);
}

public interface IIpoRepository
{
    Task InitializeAsync(CancellationToken cancellationToken = default);
    Task<IpoEvent?> GetEventAsync(string id, CancellationToken cancellationToken = default);
    Task<IpoEvent?> GetPublicEventAsync(string id, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<IpoEvent>> GetEventsAsync(DateOnly from, DateOnly to, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<IpoEvent>> GetPendingEventsAsync(DateOnly date, CancellationToken cancellationToken = default);
    Task<UpsertEventResult> UpsertEventAsync(ReconciledIpoEvent resolved, CancellationToken cancellationToken = default);
    Task SaveCollectorResultAsync(CollectorResult result, CancellationToken cancellationToken = default);
    Task SaveAnnouncementAsync(AnnouncementDocument document, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<AnnouncementDocument>> GetAnnouncementsAsync(string ipoEventId, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<SourceFieldValue>> GetFieldSourcesAsync(string ipoEventId, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<ManualOverrideEntry>> GetManualOverridesAsync(string eventId, int eventVersion, CancellationToken cancellationToken = default);
    Task AcknowledgeAsync(string eventId, int eventVersion, DateTimeOffset confirmedAt, string dataHash, CancellationToken cancellationToken = default);
    Task RevokeAcknowledgementAsync(string eventId, int eventVersion, DateTimeOffset revokedAt, CancellationToken cancellationToken = default);
    Task SetLifecycleStatusAsync(string eventId, int eventVersion, IpoLifecycleStatus status, DateTimeOffset updatedAt, CancellationToken cancellationToken = default);
    Task EnqueueRemindersAsync(IReadOnlyList<ReminderScheduleItem> reminders, CancellationToken cancellationToken = default);
    Task ReconcileReminderScheduleAsync(string eventId, int eventVersion, IReadOnlyList<ReminderScheduleItem> reminders, DateTimeOffset updatedAt, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<ReminderDelivery>> ClaimDueRemindersAsync(DateTimeOffset now, TimeSpan leaseDuration, int limit, CancellationToken cancellationToken = default);
    Task CompleteReminderAsync(long outboxId, DateTimeOffset shownAt, string deliveryChannel, CancellationToken cancellationToken = default);
    Task FailReminderAsync(long outboxId, DateTimeOffset retryAt, string error, CancellationToken cancellationToken = default);
    Task<AppSettings> GetSettingsAsync(CancellationToken cancellationToken = default);
    Task SaveSettingsAsync(AppSettings settings, CancellationToken cancellationToken = default);
    Task TouchHeartbeatAsync(string component, DateTimeOffset timestamp, CancellationToken cancellationToken = default);
    Task<HealthSummary> GetHealthSummaryAsync(DateOnly date, DateTimeOffset now, CancellationToken cancellationToken = default);
    Task<bool> TryMarkHealthSummarySentAsync(DateOnly date, DateTimeOffset sentAt, CancellationToken cancellationToken = default);
    Task AddManualOverrideAsync(string eventId, int eventVersion, string fieldName, string value, string reason, string? announcementDocumentId, CancellationToken cancellationToken = default);
    Task RevokeManualOverrideAsync(long overrideId, DateTimeOffset revokedAt, CancellationToken cancellationToken = default);
}

public interface IReminderSink
{
    Task ShowAsync(ReminderDelivery reminder, CancellationToken cancellationToken);
    Task ShowHealthSummaryAsync(HealthSummary summary, CancellationToken cancellationToken);
}

public interface ISyncTrigger
{
    void RequestSync(string reason);
}
