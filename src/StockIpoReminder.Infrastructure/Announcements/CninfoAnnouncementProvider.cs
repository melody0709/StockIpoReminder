using System.Globalization;
using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Collectors;

namespace StockIpoReminder.Infrastructure.Announcements;

public sealed class CninfoAnnouncementProvider : IAnnouncementProvider, IDisposable
{
    private static readonly Uri LandingPage = new("https://www.cninfo.com.cn/new/index");
    private static readonly Uri QueryEndpoint = new("https://www.cninfo.com.cn/new/hisAnnouncement/query");
    private static readonly TimeSpan MinimumQueryInterval = TimeSpan.FromMilliseconds(350);
    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;
    private readonly SemaphoreSlim _requestGate = new(1, 1);
    private bool _sessionInitialized;
    private DateTimeOffset? _lastQueryStartedAt;

    public CninfoAnnouncementProvider(HttpClient httpClient, TimeProvider? timeProvider = null)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider ?? TimeProvider.System;
    }

    public string ProviderName => "cninfo-announcement";
    public bool Supports(Exchange exchange) => exchange == Exchange.Shenzhen;

    public async Task<IReadOnlyList<AnnouncementReference>> SearchAsync(
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken)
    {
        await _requestGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            await EnsureSessionAsync(cancellationToken).ConfigureAwait(false);
            await ApplyQueryPacingAsync(cancellationToken).ConfigureAwait(false);

            using var body = new FormUrlEncodedContent(new Dictionary<string, string>
            {
                ["pageNum"] = "1",
                ["pageSize"] = "100",
                ["column"] = "szse",
                ["tabName"] = "fulltext",
                ["searchkey"] = ipoEvent.SecurityCode,
                ["seDate"] = $"{from:yyyy-MM-dd}~{to:yyyy-MM-dd}",
                ["plate"] = string.Empty,
                ["stock"] = string.Empty,
                ["category"] = string.Empty,
                ["trade"] = string.Empty,
                ["sortName"] = string.Empty,
                ["sortType"] = string.Empty,
            });
            using var request = new HttpRequestMessage(HttpMethod.Post, QueryEndpoint)
            {
                Content = body,
            };
            request.Headers.Referrer = LandingPage;
            _lastQueryStartedAt = _timeProvider.GetUtcNow();

            using var response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                cancellationToken).ConfigureAwait(false);
            HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
            var raw = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
            try
            {
                return Parse(raw);
            }
            catch (JsonException ex)
            {
                var mediaType = response.Content.Headers.ContentType?.MediaType ?? "<missing>";
                throw new JsonException(
                    $"巨潮公告响应契约校验失败；httpStatus={(int)response.StatusCode}；contentType={mediaType}；{ex.Message}",
                    ex);
            }
        }
        finally
        {
            _requestGate.Release();
        }
    }

    public static IReadOnlyList<AnnouncementReference> Parse(string raw)
    {
        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(raw);
        }
        catch (JsonException ex)
        {
            throw new JsonException("巨潮公告响应不是有效 JSON。", ex);
        }

        using (document)
        {
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object
                || !root.TryGetProperty("announcements", out var data))
            {
                throw new JsonException($"巨潮公告响应缺少 announcements 数组；{ContractSummary(root)}");
            }

            if (data.ValueKind == JsonValueKind.Null && IsExplicitEmptyResult(root))
            {
                return [];
            }

            if (data.ValueKind != JsonValueKind.Array)
            {
                throw new JsonException($"巨潮公告响应 announcements 不是数组，且未明确报告零条结果；{ContractSummary(root)}");
            }

            if (data.GetArrayLength() == 0 && HasPositiveCount(root))
            {
                throw new JsonException($"巨潮公告响应 announcements 为空，但总记录数大于零；{ContractSummary(root)}");
            }

            var result = new List<AnnouncementReference>();
            foreach (var item in data.EnumerateArray())
            {
                var title = Text(item, "announcementTitle");
                var path = Text(item, "adjunctUrl");
                var id = Text(item, "announcementId");
                if (title is null || path is null || id is null || !AnnouncementKeywords.IsRelevant(title))
                {
                    continue;
                }

                DateTimeOffset? published = null;
                if (item.TryGetProperty("announcementTime", out var time) && time.TryGetInt64(out var epoch))
                {
                    published = DateTimeOffset.FromUnixTimeMilliseconds(epoch);
                }

                result.Add(new AnnouncementReference
                {
                    Provider = "cninfo-announcement",
                    AnnouncementId = id,
                    Title = title,
                    Url = new Uri(new Uri("https://static.cninfo.com.cn/"), path),
                    PublishedAt = published,
                    AnnouncementType = AnnouncementKeywords.GetType(title),
                });
            }

            return result;
        }
    }

    private async Task EnsureSessionAsync(CancellationToken cancellationToken)
    {
        if (_sessionInitialized)
        {
            return;
        }

        using var request = new HttpRequestMessage(HttpMethod.Get, LandingPage);
        using var response = await _httpClient.SendAsync(
            request,
            HttpCompletionOption.ResponseHeadersRead,
            cancellationToken).ConfigureAwait(false);
        HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
        await response.Content.CopyToAsync(Stream.Null, cancellationToken).ConfigureAwait(false);
        _sessionInitialized = true;
    }

    private async Task ApplyQueryPacingAsync(CancellationToken cancellationToken)
    {
        if (_lastQueryStartedAt is not { } lastQuery)
        {
            return;
        }

        var remaining = MinimumQueryInterval - (_timeProvider.GetUtcNow() - lastQuery);
        if (remaining > TimeSpan.Zero)
        {
            await Task.Delay(remaining, cancellationToken).ConfigureAwait(false);
        }
    }

    private static string ContractSummary(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object)
        {
            return $"rootKind={root.ValueKind}";
        }

        var keys = root.EnumerateObject()
            .Select(static property => property.Name)
            .OrderBy(static name => name, StringComparer.Ordinal)
            .Take(16)
            .ToArray();
        var code = Scalar(root, "code") ?? Scalar(root, "errorCode") ?? "<missing>";
        var message = Scalar(root, "message") ?? Scalar(root, "errorMessage") ?? "<missing>";
        var totalAnnouncement = Scalar(root, "totalAnnouncement") ?? "<missing>";
        var totalRecordNum = Scalar(root, "totalRecordNum") ?? "<missing>";
        return $"rootKind=Object；topLevelKeys=[{string.Join(',', keys)}]；totalAnnouncement={Limit(totalAnnouncement)}；totalRecordNum={Limit(totalRecordNum)}；code={Limit(code)}；message={Limit(message)}";
    }

    private static bool IsExplicitEmptyResult(JsonElement root) =>
        TryInteger(root, "totalAnnouncement", out var totalAnnouncement)
        && totalAnnouncement == 0
        && TryInteger(root, "totalRecordNum", out var totalRecordNum)
        && totalRecordNum == 0;

    private static bool HasPositiveCount(JsonElement root) =>
        TryInteger(root, "totalAnnouncement", out var totalAnnouncement) && totalAnnouncement > 0
        || TryInteger(root, "totalRecordNum", out var totalRecordNum) && totalRecordNum > 0;

    private static bool TryInteger(JsonElement root, string propertyName, out int value)
    {
        value = 0;
        if (!root.TryGetProperty(propertyName, out var property))
        {
            return false;
        }

        return property.ValueKind switch
        {
            JsonValueKind.Number => property.TryGetInt32(out value),
            JsonValueKind.String => int.TryParse(property.GetString(), NumberStyles.Integer, CultureInfo.InvariantCulture, out value),
            _ => false,
        };
    }

    private static string? Scalar(JsonElement root, string propertyName)
    {
        if (!root.TryGetProperty(propertyName, out var value)
            || value.ValueKind is JsonValueKind.Object or JsonValueKind.Array or JsonValueKind.Null or JsonValueKind.Undefined)
        {
            return null;
        }

        return value.ValueKind == JsonValueKind.String ? value.GetString() : value.GetRawText();
    }

    private static string Limit(string value) => value.Length <= 160 ? value : value[..160] + "…";

    private static string? Text(JsonElement element, string property) =>
        element.TryGetProperty(property, out var value) && value.ValueKind == JsonValueKind.String
            ? Core.Services.ValueNormalizer.Text(value.GetString())
            : null;

    public void Dispose() => _requestGate.Dispose();
}
