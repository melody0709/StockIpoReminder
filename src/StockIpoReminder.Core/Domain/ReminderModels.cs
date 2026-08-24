namespace StockIpoReminder.Core.Domain;

public sealed record ReminderScheduleItem
{
    public required string IpoEventId { get; init; }
    public int EventVersion { get; init; }
    public required DateTimeOffset DueAt { get; init; }
    public required ReminderLevel Level { get; init; }
    public required string DedupeKey { get; init; }
}

public sealed record ReminderDelivery
{
    public required long OutboxId { get; init; }
    public required IpoEvent Event { get; init; }
    public required DateTimeOffset DueAt { get; init; }
    public required ReminderLevel Level { get; init; }
    public required string DedupeKey { get; init; }
    public int AttemptCount { get; init; }
}

public sealed record AppSettings
{
    public bool ShanghaiEnabled { get; init; } = true;
    public bool ShenzhenEnabled { get; init; } = true;
    public bool BeijingEnabled { get; init; } = true;
    public TimeOnly ShanghaiBrokerAcceptStart { get; init; } = new(9, 30);
    public TimeOnly ShenzhenBrokerAcceptStart { get; init; } = new(9, 15);
    public TimeOnly BeijingBrokerAcceptStart { get; init; } = new(9, 15);
    public TimeOnly SafetyCutoff { get; init; } = new(14, 55);
    public bool BeijingReservationSupported { get; init; }
    public bool SoundEnabled { get; init; } = true;
    public bool FlashTaskbar { get; init; } = true;
    public bool ToastEnabled { get; init; } = true;
    public bool DailyHealthSummaryEnabled { get; init; } = true;
    public bool AutoStartEnabled { get; init; } = true;
    public int NormalSyncMinutes { get; init; } = 30;
    public int ActiveDaySyncMinutes { get; init; } = 10;
    public bool NotificationSelfTestCompleted { get; init; }
    public bool OnboardingCompleted { get; init; }

    public bool IsExchangeEnabled(Exchange exchange) => exchange switch
    {
        Exchange.Shanghai => ShanghaiEnabled,
        Exchange.Shenzhen => ShenzhenEnabled,
        Exchange.Beijing => BeijingEnabled,
        _ => false,
    };
}

public sealed record HealthSummary
{
    public DateTimeOffset GeneratedAt { get; init; }
    public HealthState OverallState { get; init; }
    public int TodayTaskCount { get; init; }
    public int PendingConfirmationCount { get; init; }
    public int ConflictCount { get; init; }
    public int ManualReviewCount { get; init; }
    public DateTimeOffset? SchedulerHeartbeat { get; init; }
    public DateTimeOffset? DeliveryHeartbeat { get; init; }
    public IReadOnlyList<SourceHealth> Sources { get; init; } = [];
}
