using System.Globalization;
using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Collectors;

namespace StockIpoReminder.Infrastructure.Announcements;

public sealed class BseAnnouncementProvider : IAnnouncementProvider
{
    private static readonly Uri OfficialBaseUri = new("https://www.bseinfo.net/");
    private const string DetailEndpoint = "https://www.bseinfo.net/newShareController/infoDetailResult.do";
    private const string DisclosureEndpoint = "https://www.bseinfo.net/disclosureInfoController/zoneInfoResult.do";
    private const int MaximumPages = 10;

    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;

    public BseAnnouncementProvider(HttpClient httpClient, TimeProvider? timeProvider = null)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider ?? TimeProvider.System;
    }

    public string ProviderName => "bse-announcement";
    public bool Supports(Exchange exchange) => exchange == Exchange.Beijing;

    public async Task<IReadOnlyList<AnnouncementReference>> SearchAsync(
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken)
    {
        var errors = new List<Exception>();
        var detailId = TryGetDetailId(ipoEvent.AnnouncementUrl);
        if (detailId is not null)
        {
            try
            {
                var detailReferences = await SearchDetailAsync(
                    detailId,
                    ipoEvent,
                    from,
                    to,
                    cancellationToken).ConfigureAwait(false);
                if (detailReferences.Count > 0)
                {
                    return detailReferences;
                }
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                errors.Add(ex);
            }
        }

        var fallbackAttemptSucceeded = false;
        foreach (var searchTerm in GetSearchTerms(ipoEvent))
        {
            try
            {
                var references = await SearchDisclosureAsync(
                    searchTerm,
                    ipoEvent,
                    from,
                    to,
                    cancellationToken).ConfigureAwait(false);
                fallbackAttemptSucceeded = true;
                if (references.Count > 0)
                {
                    return references;
                }
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                errors.Add(ex);
            }
        }

        if (errors.Count > 0 && !fallbackAttemptSucceeded)
        {
            throw new AggregateException("北交所详情和公开发行披露检索均失败。", errors);
        }

        return [];
    }

    public static (IReadOnlyList<AnnouncementReference> References, int TotalPages) ParsePage(
        string raw,
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to)
    {
        var json = UnwrapJsonp(raw);
        using var document = JsonDocument.Parse(json);
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Array || root.GetArrayLength() == 0 || root[0].ValueKind != JsonValueKind.Object)
        {
            throw new JsonException("北交所公告响应不是非空 JSON 数组。" );
        }

        var payload = root[0];
        if (payload.TryGetProperty("newShare", out var newShare)
            && newShare.ValueKind == JsonValueKind.Object
            && !MatchesIdentity(
                JsonParsing.String(newShare, "fxCode"),
                JsonParsing.String(newShare, "stockName"),
                ipoEvent.Name,
                ipoEvent))
        {
            return ([], 1);
        }

        if (!payload.TryGetProperty("listInfo", out var listInfo)
            || listInfo.ValueKind != JsonValueKind.Object
            || !listInfo.TryGetProperty("content", out var content)
            || content.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("北交所公告响应缺少 listInfo.content。" );
        }

        var references = new List<AnnouncementReference>();
        foreach (var item in content.EnumerateArray())
        {
            var code = JsonParsing.String(item, "companyCd");
            var companyName = JsonParsing.String(item, "companyName");
            var title = JoinTitle(
                JsonParsing.String(item, "disclosureTitle"),
                JsonParsing.String(item, "disclosurePostTitle"));
            var path = JsonParsing.String(item, "destFilePath");
            var fileExtension = JsonParsing.String(item, "fileExt");
            var publishedDate = JsonParsing.Date(item, "publishDate") ?? JsonParsing.EpochDate(item, "pubDate");
            if (title is null
                || path is null
                || publishedDate is null
                || publishedDate < from
                || publishedDate > to
                || !AnnouncementKeywords.IsRelevant(title)
                || !MatchesIdentity(code, companyName, title, ipoEvent)
                || !IsPdf(path, fileExtension)
                || !TryCreateOfficialUri(path, out var url))
            {
                continue;
            }

            var pathId = Path.GetFileNameWithoutExtension(url.AbsolutePath);
            var disclosureCode = JsonParsing.String(item, "disclosureCode");
            var announcementId = ValueNormalizer.Text(pathId)
                ?? disclosureCode
                ?? ValueNormalizer.Sha256(url.AbsoluteUri)[..16];
            references.Add(new AnnouncementReference
            {
                Provider = "bse-announcement",
                AnnouncementId = announcementId,
                Title = title,
                Url = url,
                PublishedAt = ChinaTime.At(publishedDate.Value, TimeOnly.MinValue),
                AnnouncementType = AnnouncementKeywords.GetType(title),
            });
        }

        var totalPages = listInfo.TryGetProperty("totalPages", out var totalPagesElement)
            && totalPagesElement.TryGetInt32(out var parsedPages)
            ? parsedPages
            : 1;
        return (
            references
                .DistinctBy(static reference => reference.Url.AbsoluteUri, StringComparer.OrdinalIgnoreCase)
                .OrderByDescending(static reference => reference.PublishedAt)
                .ToArray(),
            Math.Max(1, totalPages));
    }

    private async Task<IReadOnlyList<AnnouncementReference>> SearchDetailAsync(
        string detailId,
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken)
    {
        var references = new List<AnnouncementReference>();
        var page = 0;
        var totalPages = 1;
        do
        {
            var uri = BuildUri(DetailEndpoint,
            [
                new("callback", "ipoDetailCb"),
                new("id", detailId),
                new("page", page.ToString(CultureInfo.InvariantCulture)),
                new("pageSize", "100"),
            ]);
            var parsed = await FetchPageAsync(uri, ipoEvent, from, to, cancellationToken).ConfigureAwait(false);
            references.AddRange(parsed.References);
            totalPages = parsed.TotalPages;
            page++;
        }
        while (page < totalPages && page < MaximumPages);

        return Deduplicate(references);
    }

    private async Task<IReadOnlyList<AnnouncementReference>> SearchDisclosureAsync(
        string searchTerm,
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken)
    {
        var references = new List<AnnouncementReference>();
        var page = 0;
        var totalPages = 1;
        do
        {
            var uri = BuildUri(DisclosureEndpoint,
            [
                new("callback", "ipoDisclosureCb"),
                new("disclosureTypes[]", "9533"),
                new("page", page.ToString(CultureInfo.InvariantCulture)),
                new("companyCd", searchTerm),
                new("startTime", from.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture)),
                new("endTime", to.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture)),
                new("keyword", string.Empty),
                new("isLink", "1"),
            ]);
            var parsed = await FetchPageAsync(uri, ipoEvent, from, to, cancellationToken).ConfigureAwait(false);
            references.AddRange(parsed.References);
            totalPages = parsed.TotalPages;
            page++;
        }
        while (page < totalPages && page < MaximumPages);

        return Deduplicate(references);
    }

    private async Task<(IReadOnlyList<AnnouncementReference> References, int TotalPages)> FetchPageAsync(
        Uri uri,
        IpoEvent ipoEvent,
        DateOnly from,
        DateOnly to,
        CancellationToken cancellationToken)
    {
        using var response = await _httpClient.GetAsync(uri, cancellationToken).ConfigureAwait(false);
        HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
        var raw = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
        return ParsePage(raw, ipoEvent, from, to);
    }

    private static IReadOnlyList<AnnouncementReference> Deduplicate(IEnumerable<AnnouncementReference> references) =>
        references
            .DistinctBy(static reference => reference.Url.AbsoluteUri, StringComparer.OrdinalIgnoreCase)
            .OrderByDescending(static reference => reference.PublishedAt)
            .ToArray();

    private static IEnumerable<string> GetSearchTerms(IpoEvent ipoEvent) =>
        new[] { ipoEvent.ApplyCode, ipoEvent.SecurityCode, ipoEvent.LegacyCode, ipoEvent.Name }
            .Select(ValueNormalizer.Text)
            .Where(static value => value is not null)
            .Select(static value => value!)
            .Distinct(StringComparer.OrdinalIgnoreCase);

    private static string? TryGetDetailId(string? announcementUrl)
    {
        if (!Uri.TryCreate(announcementUrl, UriKind.Absolute, out var uri) || !IsOfficialHost(uri.Host))
        {
            return null;
        }

        foreach (var component in uri.Query.TrimStart('?').Split('&', StringSplitOptions.RemoveEmptyEntries))
        {
            var pair = component.Split('=', 2);
            if (pair.Length == 2
                && string.Equals(Uri.UnescapeDataString(pair[0]), "id", StringComparison.OrdinalIgnoreCase))
            {
                var value = Uri.UnescapeDataString(pair[1]);
                return long.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out var parsed) && parsed > 0
                    ? value
                    : null;
            }
        }

        return null;
    }

    private static bool MatchesIdentity(string? code, string? companyName, string title, IpoEvent ipoEvent)
    {
        var normalizedCode = ValueNormalizer.Text(code);
        var knownCodes = new[] { ipoEvent.SecurityCode, ipoEvent.ApplyCode, ipoEvent.LegacyCode }
            .Select(ValueNormalizer.Text)
            .Where(static value => value is not null)
            .Select(static value => value!)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        if (normalizedCode is null || !knownCodes.Contains(normalizedCode))
        {
            return false;
        }

        var expectedName = NormalizeIdentityText(ipoEvent.Name);
        var actualName = NormalizeIdentityText(companyName);
        var normalizedTitle = NormalizeIdentityText(title);
        return expectedName.Length > 0
            && (string.Equals(actualName, expectedName, StringComparison.OrdinalIgnoreCase)
                || normalizedTitle.StartsWith(expectedName, StringComparison.OrdinalIgnoreCase));
    }

    private static string NormalizeIdentityText(string? value) =>
        new((value ?? string.Empty).Where(char.IsLetterOrDigit).ToArray());

    private static string? JoinTitle(string? title, string? postTitle)
    {
        var first = ValueNormalizer.Text(title);
        var second = ValueNormalizer.Text(postTitle);
        return first is null ? second : second is null ? first : first + second;
    }

    private static bool IsPdf(string path, string? fileExtension) =>
        string.Equals(fileExtension, "pdf", StringComparison.OrdinalIgnoreCase)
        || path.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase);

    private static bool TryCreateOfficialUri(string path, out Uri uri)
    {
        var created = Uri.TryCreate(path, UriKind.Absolute, out var absolute)
            ? absolute
            : Uri.TryCreate(OfficialBaseUri, path, out absolute)
                ? absolute
                : null;
        if (created is null
            || !string.Equals(created.Scheme, Uri.UriSchemeHttps, StringComparison.OrdinalIgnoreCase)
            || !IsOfficialHost(created.Host))
        {
            uri = null!;
            return false;
        }

        uri = created;
        return true;
    }

    private static bool IsOfficialHost(string host) =>
        string.Equals(host, "bseinfo.net", StringComparison.OrdinalIgnoreCase)
        || host.EndsWith(".bseinfo.net", StringComparison.OrdinalIgnoreCase)
        || string.Equals(host, "bse.cn", StringComparison.OrdinalIgnoreCase)
        || host.EndsWith(".bse.cn", StringComparison.OrdinalIgnoreCase);

    private static Uri BuildUri(string endpoint, IEnumerable<KeyValuePair<string, string>> query)
    {
        var builder = new UriBuilder(endpoint)
        {
            Query = string.Join('&', query.Select(static pair =>
                $"{Uri.EscapeDataString(pair.Key)}={Uri.EscapeDataString(pair.Value)}")),
        };
        return builder.Uri;
    }

    private static string UnwrapJsonp(string raw)
    {
        var trimmed = raw.Trim();
        var start = trimmed.IndexOf('(');
        var end = trimmed.LastIndexOf(')');
        if (start >= 0 && end > start)
        {
            return trimmed[(start + 1)..end];
        }

        if (trimmed.StartsWith("[", StringComparison.Ordinal))
        {
            return trimmed;
        }

        throw new JsonException("北交所公告响应不是有效 JSONP。" );
    }
}
