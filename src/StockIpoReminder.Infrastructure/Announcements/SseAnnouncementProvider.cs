using System.Globalization;
using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Collectors;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Infrastructure.Announcements;

public sealed class SseAnnouncementProvider : IAnnouncementProvider
{
    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;

    public SseAnnouncementProvider(HttpClient httpClient, TimeProvider? timeProvider = null)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider ?? TimeProvider.System;
    }

    public string ProviderName => "sse-announcement";
    public bool Supports(Exchange exchange) => exchange == Exchange.Shanghai;

    public async Task<IReadOnlyList<AnnouncementReference>> SearchAsync(
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken)
    {
        var query = new Dictionary<string, string?>
        {
            ["isPagination"] = "true",
            ["productId"] = ipoEvent.SecurityCode,
            ["keyWord"] = string.Empty,
            ["securityType"] = "0101,120100,020100,020200,120200",
            ["reportType2"] = "DQGG",
            ["reportType"] = "ALL",
            ["beginDate"] = from.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            ["endDate"] = to.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            ["pageHelp.pageSize"] = "100",
            ["pageHelp.pageNo"] = "1",
            ["pageHelp.beginPage"] = "1",
            ["pageHelp.cacheSize"] = "1",
            ["pageHelp.endPage"] = "5",
        };
        var uri = "https://query.sse.com.cn/security/stock/queryCompanyBulletin.do?" + string.Join('&', query.Select(static pair =>
            $"{Uri.EscapeDataString(pair.Key)}={Uri.EscapeDataString(pair.Value ?? string.Empty)}"));
        using var response = await _httpClient.GetAsync(uri, cancellationToken).ConfigureAwait(false);
        HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
        var raw = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
        return Parse(raw);
    }

    public static IReadOnlyList<AnnouncementReference> Parse(string raw)
    {
        using var document = JsonDocument.Parse(raw);
        if (!document.RootElement.TryGetProperty("pageHelp", out var pageHelp)
            || !pageHelp.TryGetProperty("data", out var data)
            || data.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("上交所公告响应缺少 pageHelp.data。" );
        }

        var result = new List<AnnouncementReference>();
        foreach (var item in data.EnumerateArray())
        {
            var title = Text(item, "TITLE");
            var path = Text(item, "URL");
            if (title is null || path is null || !AnnouncementKeywords.IsRelevant(title))
            {
                continue;
            }

            var date = ValueNormalizer.Date(Text(item, "SSEDATE"));
            var absolute = path.StartsWith("http", StringComparison.OrdinalIgnoreCase)
                ? path
                : new Uri(new Uri("https://www.sse.com.cn"), path).ToString();
            var officialUri = new Uri(absolute);
            OutboundNetworkPolicy.EnsureAllowedAnnouncementHttps(officialUri);
            result.Add(new AnnouncementReference
            {
                Provider = "sse-announcement",
                AnnouncementId = Path.GetFileNameWithoutExtension(path),
                Title = title,
                Url = officialUri,
                PublishedAt = date is null ? null : ChinaTime.At(date.Value, TimeOnly.MinValue),
                AnnouncementType = AnnouncementKeywords.GetType(title),
            });
        }

        return result;
    }

    private static string? Text(JsonElement element, string property) =>
        element.TryGetProperty(property, out var value) && value.ValueKind == JsonValueKind.String
            ? ValueNormalizer.Text(value.GetString())
            : null;
}
