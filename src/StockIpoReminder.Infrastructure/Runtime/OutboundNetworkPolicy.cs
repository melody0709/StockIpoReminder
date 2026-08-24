using System.Collections.Frozen;

namespace StockIpoReminder.Infrastructure.Runtime;

public static class OutboundNetworkPolicy
{
    private static readonly FrozenSet<string> AllowedHostNames = new[]
    {
        "datacenter-web.eastmoney.com",
        "query.sse.com.cn",
        "www.sse.com.cn",
        "www.cninfo.com.cn",
        "static.cninfo.com.cn",
        "disc.static.szse.cn",
        "www.bseinfo.net",
        "www.bse.cn",
        "www.microsoft.com",
        "www.cloudflare.com",
    }.ToFrozenSet(StringComparer.OrdinalIgnoreCase);

    private static readonly FrozenSet<string> AllowedAnnouncementHostNames = new[]
    {
        "www.sse.com.cn",
        "static.cninfo.com.cn",
        "disc.static.szse.cn",
        "www.bseinfo.net",
        "www.bse.cn",
    }.ToFrozenSet(StringComparer.OrdinalIgnoreCase);

    public static IReadOnlySet<string> AllowedHosts => AllowedHostNames;

    public static void EnsureAllowedHttps(Uri uri)
    {
        EnsureAllowedHttps(uri, AllowedHostNames, "公开数据源");
    }

    public static void EnsureAllowedAnnouncementHttps(Uri uri)
    {
        EnsureAllowedHttps(uri, AllowedAnnouncementHostNames, "正式公告来源");
    }

    private static void EnsureAllowedHttps(Uri uri, FrozenSet<string> allowedHosts, string description)
    {
        ArgumentNullException.ThrowIfNull(uri);
        if (!uri.IsAbsoluteUri
            || !string.Equals(uri.Scheme, Uri.UriSchemeHttps, StringComparison.OrdinalIgnoreCase)
            || !allowedHosts.Contains(uri.IdnHost))
        {
            throw new InvalidDataException($"拒绝访问未列入{description}白名单的地址：scheme={uri.Scheme}；host={uri.IdnHost}");
        }
    }
}
