namespace StockIpoReminder.Core.Domain;

public enum Exchange
{
    Unknown = 0,
    Shanghai = 1,
    Shenzhen = 2,
    Beijing = 3,
}

public enum Board
{
    Unknown = 0,
    Main = 1,
    Star = 2,
    ChiNext = 3,
    Beijing = 4,
}

public enum IssueStatus
{
    Unknown = 0,
    Upcoming = 1,
    Active = 2,
    Postponed = 3,
    Suspended = 4,
    Terminated = 5,
    Completed = 6,
}

public enum IpoLifecycleStatus
{
    Discovered = 0,
    Scheduled = 1,
    ActiveUnconfirmed = 2,
    Acknowledged = 3,
    AcknowledgedNeedsReview = 4,
    SuspendedOrCancelled = 5,
    Superseded = 6,
    ExpiredUnconfirmed = 7,
}

public enum DataQualityStatus
{
    SingleSource = 0,
    MultiSourceVerified = 1,
    AnnouncementVerified = 2,
    DataConflict = 3,
    Stale = 4,
    ManualReviewRequired = 5,
}

public enum FundingMode
{
    MarketValue = 0,
    FullCash = 1,
}

public enum ReminderLevel
{
    Advance = 0,
    Morning = 10,
    BrokerOpening = 15,
    MarketOpening = 20,
    Hourly = 30,
    NoonBoundary = 40,
    AfternoonOpening = 45,
    FifteenMinutes = 50,
    FiveMinutes = 60,
    TwoMinutes = 70,
    Final = 80,
    DataChanged = 90,
    HealthWarning = 100,
}

public enum ReminderDeliveryState
{
    Pending = 0,
    Leased = 1,
    Delivered = 2,
    Collapsed = 3,
    Cancelled = 4,
    Failed = 5,
}

public enum HealthState
{
    Unknown = 0,
    Healthy = 1,
    Warning = 2,
    Failed = 3,
}

public enum ExtractionStatus
{
    Pending = 0,
    Extracted = 1,
    LowConfidence = 2,
    Failed = 3,
    Unsupported = 4,
}
