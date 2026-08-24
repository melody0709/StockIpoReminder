using System.Net.Http.Json;
using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Collectors;

public sealed class EastmoneyCollector : IIpoCollector
{
    private const string Endpoint = "https://datacenter-web.eastmoney.com/api/data/v1/get";
    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;

    public EastmoneyCollector(HttpClient httpClient, TimeProvider timeProvider)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider;
    }

    public string SourceName => "eastmoney";
    public int Priority => 100;

    public async Task<CollectorResult> CollectAsync(CancellationToken cancellationToken)
    {
        var started = ChinaTime.Now(_timeProvider);
        try
        {
            var uri = $"{Endpoint}?reportName=RPTA_APP_IPOAPPLY&columns=ALL&sortColumns=APPLY_DATE%2CSECURITY_CODE&sortTypes=-1%2C-1&pageNumber=1&pageSize=500&source=WEB&client=WEB";
            using var response = await _httpClient.GetAsync(uri, cancellationToken).ConfigureAwait(false);
            HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
            var raw = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
            var candidates = Parse(raw, started);
            return new CollectorResult
            {
                Source = SourceName,
                Success = true,
                StartedAt = started,
                FinishedAt = ChinaTime.Now(_timeProvider),
                Candidates = candidates,
                RawPayload = raw,
                RawHash = ValueNormalizer.Sha256(raw),
                SchemaFingerprint = GetSchemaFingerprint(raw),
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
        if (!document.RootElement.TryGetProperty("success", out var success) || success.ValueKind == JsonValueKind.False)
        {
            throw new JsonException("东方财富响应 success=false。" );
        }

        if (!document.RootElement.TryGetProperty("result", out var result)
            || result.ValueKind == JsonValueKind.Null
            || !result.TryGetProperty("data", out var data)
            || data.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("东方财富响应缺少 result.data。" );
        }

        var today = DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(fetchedAt, ChinaTime.Zone).DateTime);
        var candidates = new List<IpoCandidate>();
        foreach (var item in data.EnumerateArray())
        {
            var securityCode = JsonParsing.String(item, "SECURITY_CODE");
            var name = JsonParsing.String(item, "SECURITY_NAME");
            if (securityCode is null || name is null)
            {
                continue;
            }

            var market = JsonParsing.String(item, "MARKET_TYPE_NEW");
            var isBeijing = JsonParsing.Integer(item, "IS_BEIJING") == 1;
            var exchange = JsonParsing.DetectExchange(securityCode, market, isBeijing);
            var applyDate = JsonParsing.Date(item, "APPLY_DATE");
            var issueState = JsonParsing.String(item, "ISSUE_STATE");
            var status = issueState switch
            {
                "2" or "暂停发行" or "暂缓发行" => IssueStatus.Suspended,
                "3" or "终止发行" => IssueStatus.Terminated,
                _ => JsonParsing.StatusFromDates(applyDate, today),
            };

            candidates.Add(new IpoCandidate
            {
                Source = "eastmoney",
                SourcePriority = 100,
                FetchedAt = fetchedAt,
                Exchange = exchange,
                Board = JsonParsing.DetectBoard(exchange, securityCode, market),
                SecurityCode = securityCode,
                ApplyCode = JsonParsing.String(item, "APPLY_CODE"),
                Name = name,
                ApplyDate = applyDate,
                IssuePrice = JsonParsing.Decimal(item, "ISSUE_PRICE", zeroMeansMissing: true),
                LotSize = JsonParsing.Integer(item, "EACHBALLOT_SHARES", zeroMeansMissing: true),
                MaxApplyQuantity = JsonParsing.Integer(item, "ONLINE_APPLY_UPPER", zeroMeansMissing: true),
                RequiredMarketValue = JsonParsing.Decimal(item, "TOP_APPLY_MARKETCAP", zeroMeansMissing: true),
                BallotDate = JsonParsing.Date(item, "BALLOT_NUM_DATE"),
                PaymentDate = JsonParsing.Date(item, "BALLOT_PAY_DATE"),
                ListingDate = JsonParsing.Date(item, "LISTING_DATE"),
                Status = status,
            });
        }

        return candidates;
    }

    private static string GetSchemaFingerprint(string raw)
    {
        using var document = JsonDocument.Parse(raw);
        var data = document.RootElement.GetProperty("result").GetProperty("data");
        return JsonParsing.SchemaFingerprint(data.EnumerateArray());
    }
}
