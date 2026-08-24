using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Collectors;

public sealed class CninfoCollector : IIpoCollector
{
    private const string Endpoint = "https://www.cninfo.com.cn/neweipo/index/ipoListQuery";
    private readonly HttpClient _httpClient;
    private readonly TimeProvider _timeProvider;

    public CninfoCollector(HttpClient httpClient, TimeProvider timeProvider)
    {
        _httpClient = httpClient;
        _timeProvider = timeProvider;
    }

    public string SourceName => "cninfo";
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
            var data = document.RootElement.GetProperty("data");
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
        if (!document.RootElement.TryGetProperty("code", out var code) || code.GetInt32() != 200
            || !document.RootElement.TryGetProperty("data", out var data)
            || data.ValueKind != JsonValueKind.Array)
        {
            throw new JsonException("巨潮响应状态异常或缺少 data。" );
        }

        var today = DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(fetchedAt, ChinaTime.Zone).DateTime);
        var candidates = new List<IpoCandidate>();
        foreach (var item in data.EnumerateArray())
        {
            var securityCode = JsonParsing.String(item, "obSecCode0007");
            var name = JsonParsing.String(item, "obSecName0007");
            if (securityCode is null || name is null)
            {
                continue;
            }

            var applyDate = JsonParsing.Date(item, "f035d0089Date");
            candidates.Add(new IpoCandidate
            {
                Source = "cninfo",
                SourcePriority = 200,
                FetchedAt = fetchedAt,
                Exchange = Exchange.Shenzhen,
                Board = JsonParsing.DetectBoard(Exchange.Shenzhen, securityCode, null),
                SecurityCode = securityCode,
                ApplyCode = securityCode,
                Name = name,
                ApplyDate = applyDate,
                IssuePrice = JsonParsing.Decimal(item, "f008n0089", zeroMeansMissing: true),
                MaxApplyQuantity = JsonParsing.Integer(item, "f042n0089", 10_000m, zeroMeansMissing: true),
                BallotDate = JsonParsing.Date(item, "f108d0089"),
                ListingDate = JsonParsing.Date(item, "f007d0007"),
                Status = JsonParsing.StatusFromDates(applyDate, today),
            });
        }

        return candidates;
    }
}
