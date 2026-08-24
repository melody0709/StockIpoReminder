using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Collectors;

public sealed class SseCollector : IIpoCollector
{
    private const string Endpoint = "https://query.sse.com.cn/commonQuery.do?sqlId=COMMON_SSE_IPO_IPO_LIST_L&isPagination=true&pageHelp.pageNo=1&pageHelp.pageSize=500";
    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;

    public SseCollector(HttpClient httpClient, TimeProvider timeProvider)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider;
    }

    public string SourceName => "sse";
    public int Priority => 200;

    public async Task<CollectorResult> CollectAsync(CancellationToken cancellationToken)
    {
        var started = ChinaTime.Now(_timeProvider);
        try
        {
            using var response = await _httpClient.GetAsync(Endpoint, cancellationToken).ConfigureAwait(false);
            HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
            var raw = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
            var candidates = Parse(raw, started);
            using var document = JsonDocument.Parse(raw);
            var data = document.RootElement.GetProperty("pageHelp").GetProperty("data");
            return new CollectorResult
            {
                Source = SourceName,
                Success = true,
                StartedAt = started,
                FinishedAt = ChinaTime.Now(_timeProvider),
                Candidates = candidates,
                RawPayload = raw,
                RawHash = ValueNormalizer.Sha256(raw),
                SchemaFingerprint = JsonParsing.SchemaFingerprint(data.EnumerateArray()),
                RecordCount = candidates.Count,
            };
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            return CollectorResult.Failed(SourceName, started, ChinaTime.Now(_timeProvider), ex);
        }
    }

    public static IReadOnlyList<IpoCandidate> Parse(string raw, DateTimeOffset fetchedAt)
    {
        using var document = JsonDocument.Parse(raw);
        if (!document.RootElement.TryGetProperty("pageHelp", out var pageHelp)
            || !pageHelp.TryGetProperty("data", out var data)
            || data.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("上交所响应缺少 pageHelp.data。" );
        }

        var today = DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(fetchedAt, ChinaTime.Zone).DateTime);
        var candidates = new List<IpoCandidate>();
        foreach (var item in data.EnumerateArray())
        {
            var code = JsonParsing.String(item, "SECURITY_CODE");
            var name = JsonParsing.String(item, "SECURITY_NAME");
            if (code is null || name is null)
            {
                continue;
            }

            var applyDate = JsonParsing.Date(item, "ONLINE_ISSUANCE_DATE");
            var rawStatus = JsonParsing.String(item, "IPO_OVERALL_STATUS");
            var status = rawStatus switch
            {
                "3" or "4" => IssueStatus.Terminated,
                _ => JsonParsing.StatusFromDates(applyDate, today),
            };

            candidates.Add(new IpoCandidate
            {
                Source = "sse",
                SourcePriority = 200,
                FetchedAt = fetchedAt,
                Exchange = Exchange.Shanghai,
                Board = JsonParsing.DetectBoard(Exchange.Shanghai, code, null),
                SecurityCode = code,
                Name = name,
                ApplyDate = applyDate,
                IssuePrice = JsonParsing.Decimal(item, "ISSUE_PRICE", zeroMeansMissing: true),
                MaxApplyQuantity = JsonParsing.Integer(item, "ONLINE_PURCHASE_LIMIT", 10_000m, zeroMeansMissing: true),
                PaymentDate = JsonParsing.Date(item, "PAYMENT_START_DATE"),
                ListingDate = JsonParsing.Date(item, "LISTED_DATE"),
                Status = status,
                AnnouncementUrl = JsonParsing.String(item, "ANNOUNCEMENT_URL"),
            });
        }

        return candidates;
    }
}
