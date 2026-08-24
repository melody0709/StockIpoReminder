using System.Net;
using System.Net.Http.Headers;
using System.Text.Json;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Collectors;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class CollectorFixtureTests
{
    private static readonly DateTimeOffset FetchedAt = new(2026, 8, 24, 3, 0, 0, TimeSpan.Zero);

    [TestMethod]
    public void Eastmoney_Real_Fixture_Maps_Fields_And_Missing_Values()
    {
        var candidates = EastmoneyCollector.Parse(FixtureLoader.Read("Collectors/eastmoney-20260824.json"), FetchedAt);

        var item = AssertExactlyOne(candidates);
        Assert.AreEqual(Exchange.Shenzhen, item.Exchange);
        Assert.AreEqual(Board.ChiNext, item.Board);
        Assert.AreEqual("301689", item.SecurityCode);
        Assert.AreEqual("301689", item.ApplyCode);
        Assert.AreEqual("电科思仪", item.Name);
        Assert.AreEqual(new DateOnly(2026, 8, 28), item.ApplyDate);
        Assert.IsNull(item.IssuePrice);
        Assert.AreEqual(500, item.LotSize);
        Assert.AreEqual(12000, item.MaxApplyQuantity);
        Assert.AreEqual(12m, item.RequiredMarketValue);
        Assert.AreEqual(IssueStatus.Upcoming, item.Status);
    }

    [TestMethod]
    public void Sse_Real_Fixture_Converts_TenThousand_Share_Limit_And_Detects_Board()
    {
        var candidates = SseCollector.Parse(FixtureLoader.Read("Collectors/sse-20260824.json"), FetchedAt);

        Assert.AreEqual(2, candidates.Count);
        var main = candidates.Single(item => item.SecurityCode == "601123");
        var star = candidates.Single(item => item.SecurityCode == "688835");
        Assert.AreEqual(Board.Main, main.Board);
        Assert.AreEqual(29500, main.MaxApplyQuantity);
        Assert.AreEqual(6.65m, main.IssuePrice);
        Assert.IsNull(main.AnnouncementUrl);
        Assert.AreEqual(Board.Star, star.Board);
        Assert.AreEqual(5500, star.MaxApplyQuantity);
        Assert.AreEqual(new DateOnly(2026, 8, 25), star.ListingDate);
    }

    [TestMethod]
    public void Cninfo_Real_Fixture_Preserves_Known_Date_When_Optional_Fields_Are_Null()
    {
        var candidates = CninfoCollector.Parse(FixtureLoader.Read("Collectors/cninfo-20260824.json"), FetchedAt);

        var item = AssertExactlyOne(candidates);
        Assert.AreEqual("301689", item.SecurityCode);
        Assert.AreEqual(new DateOnly(2026, 8, 28), item.ApplyDate);
        Assert.IsNull(item.IssuePrice);
        Assert.IsNull(item.MaxApplyQuantity);
        Assert.AreEqual(new DateOnly(2026, 9, 1), item.BallotDate);
        Assert.AreEqual(IssueStatus.Upcoming, item.Status);
    }

    [TestMethod]
    public void Bse_Real_Fixture_Uses_Epoch_Dates_And_Preserves_Legacy_Code()
    {
        var (candidates, totalPages) = BseCollector.ParsePage(
            FixtureLoader.Read("Collectors/bse-page0-20260824.jsonp"),
            FetchedAt);

        var item = AssertExactlyOne(candidates);
        Assert.AreEqual(2, totalPages);
        Assert.AreEqual("920289", item.SecurityCode);
        Assert.AreEqual("920289", item.ApplyCode);
        Assert.AreEqual("874378", item.LegacyCode);
        Assert.AreEqual("华汇智能", item.Name);
        Assert.AreEqual(new DateOnly(2026, 8, 24), item.ApplyDate);
        Assert.AreEqual(new DateOnly(2026, 8, 27), item.BallotDate);
        Assert.AreEqual(17.71m, item.IssuePrice);
        Assert.AreEqual(IssueStatus.Active, item.Status);
        StringAssert.Contains(item.AnnouncementUrl, "id=346");
    }

    [TestMethod]
    public async Task Collectors_Produce_Hashes_And_Schema_Fingerprints_From_Fixtures()
    {
        var cases = new (Func<HttpClient, object> Create, string Fixture, string MediaType)[]
        {
            (client => new EastmoneyCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt)), "Collectors/eastmoney-20260824.json", "text/plain"),
            (client => new SseCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt)), "Collectors/sse-20260824.json", "application/json"),
            (client => new CninfoCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt)), "Collectors/cninfo-20260824.json", "application/json"),
        };

        foreach (var testCase in cases)
        {
            var raw = FixtureLoader.Read(testCase.Fixture);
            using var handler = new StubHttpMessageHandler((_, _) => Task.FromResult(StubHttpMessageHandler.Text(raw, testCase.MediaType)));
            using var client = new HttpClient(handler);
            var result = testCase.Create(client) switch
            {
                EastmoneyCollector collector => await collector.CollectAsync(CancellationToken.None),
                SseCollector collector => await collector.CollectAsync(CancellationToken.None),
                CninfoCollector collector => await collector.CollectAsync(CancellationToken.None),
                _ => throw new InvalidOperationException(),
            };

            Assert.IsTrue(result.Success, result.Error);
            Assert.IsGreaterThan(0, result.RecordCount);
            Assert.AreEqual(raw, result.RawPayload);
            Assert.AreEqual(64, result.RawHash?.Length);
            Assert.AreEqual(64, result.SchemaFingerprint?.Length);
        }
    }

    [TestMethod]
    public async Task Bse_Collector_Establishes_Session_And_Traverses_All_Pages()
    {
        var page0 = FixtureLoader.Read("Collectors/bse-page0-20260824.jsonp");
        var page1 = FixtureLoader.Read("Collectors/bse-page1-20260824.jsonp");
        using var handler = new StubHttpMessageHandler(async (request, cancellationToken) =>
        {
            if (request.Method == HttpMethod.Get)
            {
                return StubHttpMessageHandler.Text("<html>landing</html>", "text/html");
            }

            var body = await request.Content!.ReadAsStringAsync(cancellationToken);
            return StubHttpMessageHandler.Text(body.Contains("page=1", StringComparison.Ordinal) ? page1 : page0, "text/html");
        });
        using var client = new HttpClient(handler);
        var collector = new BseCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt));

        var result = await collector.CollectAsync(CancellationToken.None);

        Assert.IsTrue(result.Success, result.Error);
        Assert.AreEqual(2, result.RecordCount);
        CollectionAssert.AreEquivalent(new[] { "920289", "920071" }, result.Candidates.Select(item => item.ApplyCode).ToArray());
        Assert.AreEqual(3, handler.Requests.Count);
        Assert.AreEqual(HttpMethod.Get, handler.Requests[0].Method);
        StringAssert.Contains(handler.Requests[1].Body, "page=0");
        StringAssert.Contains(handler.Requests[2].Body, "page=1");
        Assert.AreEqual(64, result.SchemaFingerprint?.Length);
    }

    [TestMethod]
    public async Task Bse_Schema_Fingerprint_Changes_When_Content_Fields_Change()
    {
        var original = FixtureLoader.Read("Collectors/bse-page0-20260824.jsonp");
        var changed = original.Replace("\"stockName\":\"华汇智能\"", "\"stockName\":\"华汇智能\",\"unexpectedField\":\"x\"", StringComparison.Ordinal);

        var originalResult = await CollectSingleBsePageAsync(original);
        var changedResult = await CollectSingleBsePageAsync(changed);

        Assert.AreNotEqual(originalResult.SchemaFingerprint, changedResult.SchemaFingerprint);
    }

    [TestMethod]
    public async Task Html_Error_Page_Is_A_Failed_Collection_Not_A_Healthy_Empty_Result()
    {
        using var handler = new StubHttpMessageHandler((_, _) =>
            Task.FromResult(StubHttpMessageHandler.Text("<html>403 Forbidden</html>", "text/html", HttpStatusCode.OK)));
        using var client = new HttpClient(handler);
        var collector = new EastmoneyCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt));

        var result = await collector.CollectAsync(CancellationToken.None);

        Assert.IsFalse(result.Success);
        Assert.AreEqual(0, result.RecordCount);
        StringAssert.Contains(result.Error, "Json");
    }

    [TestMethod]
    public async Task Http_429_Delta_RetryAfter_Is_Propagated_To_Collector_Result()
    {
        using var handler = new StubHttpMessageHandler((_, _) =>
        {
            var response = StubHttpMessageHandler.Text("rate limited", "text/plain", HttpStatusCode.TooManyRequests);
            response.Headers.RetryAfter = new RetryConditionHeaderValue(TimeSpan.FromMinutes(10));
            return Task.FromResult(response);
        });
        using var client = new HttpClient(handler);
        var collector = new EastmoneyCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt));

        var result = await collector.CollectAsync(CancellationToken.None);

        Assert.IsFalse(result.Success);
        Assert.AreEqual(TimeSpan.FromMinutes(10), result.RetryAfter);
        StringAssert.Contains(result.Error, "429");
    }

    [TestMethod]
    public async Task Http_503_Date_RetryAfter_Is_Propagated_To_Collector_Result()
    {
        using var handler = new StubHttpMessageHandler((_, _) =>
        {
            var response = StubHttpMessageHandler.Text("unavailable", "text/plain", HttpStatusCode.ServiceUnavailable);
            response.Headers.RetryAfter = new RetryConditionHeaderValue(FetchedAt.AddMinutes(12));
            return Task.FromResult(response);
        });
        using var client = new HttpClient(handler);
        var collector = new EastmoneyCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt));

        var result = await collector.CollectAsync(CancellationToken.None);

        Assert.IsFalse(result.Success);
        Assert.AreEqual(TimeSpan.FromMinutes(12), result.RetryAfter);
        StringAssert.Contains(result.Error, "503");
    }

    [TestMethod]
    public void Malformed_Or_Business_Error_Responses_Are_Rejected()
    {
        Assert.ThrowsExactly<JsonException>(() => EastmoneyCollector.Parse("{\"success\":false}", FetchedAt));
        Assert.ThrowsExactly<JsonException>(() => SseCollector.Parse("{\"pageHelp\":{}}", FetchedAt));
        Assert.ThrowsExactly<JsonException>(() => CninfoCollector.Parse("{\"code\":500,\"data\":[]}", FetchedAt));
        Assert.ThrowsExactly<JsonException>(() => BseCollector.ParsePage("not-jsonp", FetchedAt));
    }

    private static async Task<CollectorResult> CollectSingleBsePageAsync(string jsonp)
    {
        var onePage = jsonp.Replace("\"totalPages\":2", "\"totalPages\":1", StringComparison.Ordinal);
        using var handler = new StubHttpMessageHandler((request, _) => Task.FromResult(
            request.Method == HttpMethod.Get
                ? StubHttpMessageHandler.Text("<html>landing</html>", "text/html")
                : StubHttpMessageHandler.Text(onePage, "text/html")));
        using var client = new HttpClient(handler);
        var collector = new BseCollector(client, new RepositoryTestContext.MutableTimeProvider(FetchedAt));
        return await collector.CollectAsync(CancellationToken.None);
    }

    private static T AssertExactlyOne<T>(IReadOnlyList<T> values)
    {
        Assert.AreEqual(1, values.Count);
        return values[0];
    }
}
