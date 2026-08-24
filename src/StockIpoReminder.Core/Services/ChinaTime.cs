namespace StockIpoReminder.Core.Services;

public static class ChinaTime
{
    public static TimeZoneInfo Zone { get; } = ResolveZone();

    public static DateTimeOffset Now(TimeProvider timeProvider) =>
        TimeZoneInfo.ConvertTime(timeProvider.GetUtcNow(), Zone);

    public static DateTimeOffset At(DateOnly date, TimeOnly time)
    {
        var unspecified = date.ToDateTime(time, DateTimeKind.Unspecified);
        var offset = Zone.GetUtcOffset(unspecified);
        return new DateTimeOffset(unspecified, offset);
    }

    public static DateOnly Today(TimeProvider timeProvider) => DateOnly.FromDateTime(Now(timeProvider).DateTime);

    private static TimeZoneInfo ResolveZone()
    {
        foreach (var id in new[] { "China Standard Time", "Asia/Shanghai" })
        {
            try
            {
                return TimeZoneInfo.FindSystemTimeZoneById(id);
            }
            catch (TimeZoneNotFoundException)
            {
            }
        }

        return TimeZoneInfo.CreateCustomTimeZone("Asia/Shanghai", TimeSpan.FromHours(8), "Asia/Shanghai", "Asia/Shanghai");
    }
}
