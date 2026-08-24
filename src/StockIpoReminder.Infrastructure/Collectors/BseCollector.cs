using System.Net;
using System.Text;
using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Collectors;

public sealed class BseCollector : IIpoCollector
{
    private const string LandingPage = "https://www.bseinfo.net/newshare/listofissues.html";
    private const string Endpoint = "https://www.bseinfo.net/newShareController/infoResult.do?callback=ipoCb";
    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;

    public BseCollector(HttpClient httpClient, TimeProvider timeProvider)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider;
    }

    public string SourceName => "bse";
    public int Priority => 200;

    public async Task<CollectorResult> CollectAsync(CancellationToken cancellationToken)
    {
        var started = ChinaTime.Now(_timeProvider);
        try
        {
            using (var landing = await _httpClient.GetAsync(LandingPage, cancellationToken).ConfigureAwait(false))
            {
                HttpResponseGuard.EnsureSuccess(landing, ChinaTime.Now(_timeProvider));
            }

            var pages = new List<string>();
            var allCandidates = new List<IpoCandidate>();
            var page = 0;
            var totalPages = 1;
            do
            {
                using var body = new FormUrlEncodedContent(new Dictionary<string, string>
                {
                    ["statetypes"] = "1",
                    ["page"] = page.ToString(System.Globalization.CultureInfo.InvariantCulture),
                    ["isNewThree"] = "1",
                    ["sortfield"] = "purchaseDate",
                    ["sorttype"] = "desc",
                });
                using var response = await _httpClient.PostAsync(Endpoint, body, cancellationToken).ConfigureAwait(false);
                HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
                var raw = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
                pages.Add(raw);
                var parsed = ParsePage(raw, started);
                allCandidates.AddRange(parsed.Candidates);
                totalPages = parsed.TotalPages;
                page++;
            }
            while (page < totalPages && page < 50);

            var combined = string.Join("\n", pages);
            var schemaFingerprint = ValueNormalizer.Sha256(string.Join(
                "\n",
                pages.Select(GetSchemaFingerprint).Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal)));
            return new CollectorResult
            {
                Source = SourceName,
                Success = true,
                StartedAt = started,
                FinishedAt = ChinaTime.Now(_timeProvider),
                Candidates = allCandidates,
                RawPayload = combined,
                RawHash = ValueNormalizer.Sha256(combined),
                SchemaFingerprint = schemaFingerprint,
                RecordCount = allCandidates.Count,
            };
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            return CollectorResult.Failed(SourceName, started, ChinaTime.Now(_timeProvider), ex);
        }
    }

    public static (IReadOnlyList<IpoCandidate> Candidates, int TotalPages) ParsePage(string jsonp, DateTimeOffset fetchedAt)
    {
        var json = UnwrapJsonp(jsonp);
        using var document = JsonDocument.Parse(json);
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Array || root.GetArrayLength() == 0
            || !root[0].TryGetProperty("listInfo", out var listInfo)
            || !listInfo.TryGetProperty("content", out var content)
            || content.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("北交所 JSONP 响应缺少 listInfo.content。" );
        }

        var today = DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(fetchedAt, ChinaTime.Zone).DateTime);
        var candidates = new List<IpoCandidate>();
        foreach (var item in content.EnumerateArray())
        {
            var issueCode = JsonParsing.String(item, "fxCode");
            var legacyCode = JsonParsing.String(item, "stockCode");
            var name = JsonParsing.String(item, "stockName");
            if (issueCode is null || name is null)
            {
                continue;
            }

            var applyDate = JsonParsing.EpochDate(item, "purchaseDate");
            var suspended = JsonParsing.EpochDate(item, "suspendDate") is not null;
            var terminated = JsonParsing.EpochDate(item, "terminationDate") is not null;
            var id = JsonParsing.String(item, "id");
            candidates.Add(new IpoCandidate
            {
                Source = "bse",
                SourcePriority = 200,
                FetchedAt = fetchedAt,
                Exchange = Exchange.Beijing,
                Board = Board.Beijing,
                SecurityCode = issueCode,
                ApplyCode = issueCode,
                LegacyCode = legacyCode,
                Name = name,
                ApplyDate = applyDate,
                IssuePrice = JsonParsing.Decimal(item, "issuePrice", zeroMeansMissing: true),
                RequiredCash = null,
                BallotDate = JsonParsing.EpochDate(item, "issueResultDate"),
                ListingDate = JsonParsing.EpochDate(item, "enterPremiumDate"),
                Status = JsonParsing.StatusFromDates(applyDate, today, suspended, terminated),
                AnnouncementUrl = id is null ? null : $"https://www.bseinfo.net/newshare/listofissues_detail.html?id={Uri.EscapeDataString(id)}",
            });
        }

        var totalPages = listInfo.TryGetProperty("totalPages", out var totalPagesElement) && totalPagesElement.TryGetInt32(out var parsedPages)
            ? parsedPages
            : 1;
        return (candidates, Math.Max(1, totalPages));
    }

    private static string UnwrapJsonp(string value)
    {
        var trimmed = value.Trim();
        var start = trimmed.IndexOf('(');
        var end = trimmed.LastIndexOf(')');
        if (start < 0 || end <= start)
        {
            throw new JsonException("北交所响应不是有效 JSONP。" );
        }

        return trimmed[(start + 1)..end];
    }

    private static string GetSchemaFingerprint(string jsonp)
    {
        using var document = JsonDocument.Parse(UnwrapJsonp(jsonp));
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Array || root.GetArrayLength() == 0
            || !root[0].TryGetProperty("listInfo", out var listInfo)
            || !listInfo.TryGetProperty("content", out var content)
            || content.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("北交所 JSONP 响应缺少 listInfo.content。" );
        }

        return JsonParsing.SchemaFingerprint(content.EnumerateArray());
    }
}
