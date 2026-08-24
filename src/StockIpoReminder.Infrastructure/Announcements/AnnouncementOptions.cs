namespace StockIpoReminder.Infrastructure.Announcements;

public sealed record AnnouncementOptions
{
    public required string StorageDirectory { get; init; }
}
