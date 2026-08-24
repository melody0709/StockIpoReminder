namespace StockIpoReminder.Infrastructure.Announcements;

internal static class AnnouncementKeywords
{
    private static readonly string[] Keywords =
    [
        "发行安排及初步询价公告",
        "发行公告",
        "暂缓发行",
        "中止发行",
        "终止发行",
        "延期发行",
        "重新启动发行",
        "网上发行申购情况及中签率公告",
        "网上中签结果公告",
        "上市公告书",
    ];

    public static bool IsRelevant(string title) => Keywords.Any(title.Contains);

    public static string? GetType(string title) => Keywords.FirstOrDefault(title.Contains);
}
