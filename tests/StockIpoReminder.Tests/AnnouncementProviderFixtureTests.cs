using System.Text.Json;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Announcements;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class AnnouncementProviderFixtureTests
{
    [TestMethod]
    public void Sse_Provider_Parses_Relevant_Official_Pdf_And_Filters_Unrelated_Items()
    {
        var references = SseAnnouncementProvider.Parse(FixtureLoader.Read("Announcements/sse-601123-20260824.json"));

        var item = AssertExactlyOne(references);
        Assert.AreEqual("sse-announcement", item.Provider);
        Assert.AreEqual("601123_20260820_HUOS", item.AnnouncementId);
        Assert.AreEqual("发行公告", item.AnnouncementType);
        Assert.AreEqual(new DateOnly(2026, 8, 20), DateOnly.FromDateTime(item.PublishedAt!.Value.DateTime));
        Assert.AreEqual("https://www.sse.com.cn/disclosure/listedinfo/announcement/c/new/2026-08-20/601123_20260820_HUOS.pdf", item.Url.ToString());
    }

    [TestMethod]
    public void Cninfo_Provider_Parses_Relevant_Official_Pdf_And_Epoch_Time()
    {
        var references = CninfoAnnouncementProvider.Parse(FixtureLoader.Read("Announcements/cninfo-301688-20260824.json"));

        var item = AssertExactlyOne(references);
        Assert.AreEqual("cninfo-announcement", item.Provider);
        Assert.AreEqual("1225479053", item.AnnouncementId);
        Assert.AreEqual("发行公告", item.AnnouncementType);
        var localPublishedAt = TimeZoneInfo.ConvertTime(item.PublishedAt!.Value, ChinaTime.Zone);
        Assert.AreEqual(new DateOnly(2026, 8, 19), DateOnly.FromDateTime(localPublishedAt.DateTime));
        Assert.AreEqual("https://static.cninfo.com.cn/finalpage/2026-08-19/1225479053.PDF", item.Url.ToString());
    }

    [TestMethod]
    public void Cninfo_Provider_Accepts_Explicit_Zero_Result_When_Announcements_Is_Null()
    {
        var references = CninfoAnnouncementProvider.Parse(
            FixtureLoader.Read("Announcements/cninfo-301689-empty-20260824.json"));

        Assert.AreEqual(0, references.Count);
    }

    [TestMethod]
    public void Cninfo_Provider_Rejects_Null_Announcements_When_Count_Is_Nonzero_Without_Leaking_Body()
    {
        const string raw = """
            {
              "announcements": null,
              "totalAnnouncement": 3,
              "totalRecordNum": 3,
              "code": "BUSINESS_LIMIT",
              "message": "请求暂不可用",
              "debug": "raw-secret-marker"
            }
            """;

        var error = Assert.ThrowsExactly<JsonException>(() => CninfoAnnouncementProvider.Parse(raw));

        StringAssert.Contains(error.Message, "announcements 不是数组");
        StringAssert.Contains(error.Message, "totalAnnouncement=3");
        StringAssert.Contains(error.Message, "code=BUSINESS_LIMIT");
        Assert.IsFalse(error.ToString().Contains("raw-secret-marker", StringComparison.Ordinal));
    }

    [TestMethod]
    public async Task Cninfo_Provider_Warms_Session_Before_Posting_Query()
    {
        using var handler = new StubHttpMessageHandler((request, _) => Task.FromResult(
            request.Method == HttpMethod.Get
                ? StubHttpMessageHandler.Text("<html><body>cninfo</body></html>", "text/html")
                : StubHttpMessageHandler.Text(FixtureLoader.Read("Announcements/cninfo-301688-20260824.json"))));
        using var client = new HttpClient(handler);
        var provider = new CninfoAnnouncementProvider(client);

        var references = await provider.SearchAsync(
            ShenzhenEvent(),
            new DateOnly(2026, 6, 25),
            new DateOnly(2026, 8, 25),
            CancellationToken.None);

        AssertExactlyOne(references);
        Assert.AreEqual(2, handler.Requests.Count);
        Assert.AreEqual(HttpMethod.Get, handler.Requests[0].Method);
        Assert.AreEqual("/new/index", handler.Requests[0].Uri!.AbsolutePath);
        Assert.AreEqual(HttpMethod.Post, handler.Requests[1].Method);
        Assert.AreEqual("/new/hisAnnouncement/query", handler.Requests[1].Uri!.AbsolutePath);
        StringAssert.Contains(handler.Requests[1].Body!, "searchkey=301688");
    }

    [TestMethod]
    public void Bse_Detail_Parser_Selects_Only_The_Matching_Official_Issue_Announcement()
    {
        var parsed = BseAnnouncementProvider.ParsePage(
            FixtureLoader.Read("Announcements/bse-detail-920289-20260824.jsonp"),
            BseEvent(),
            new DateOnly(2026, 8, 19),
            new DateOnly(2026, 8, 25));

        var item = AssertExactlyOne(parsed.References);
        Assert.AreEqual(1, parsed.TotalPages);
        Assert.AreEqual("bse-announcement", item.Provider);
        Assert.AreEqual("1787216709992_052262", item.AnnouncementId);
        Assert.AreEqual("发行公告", item.AnnouncementType);
        Assert.AreEqual(new DateOnly(2026, 8, 20), DateOnly.FromDateTime(item.PublishedAt!.Value.DateTime));
        Assert.AreEqual("https://www.bseinfo.net/disclosure/2026/2026-08-20/1787216709992_052262.pdf", item.Url.ToString());
    }

    [TestMethod]
    public void Bse_Detail_Parser_Rejects_A_Different_Issuer_Identity()
    {
        var parsed = BseAnnouncementProvider.ParsePage(
            FixtureLoader.Read("Announcements/bse-detail-920289-20260824.jsonp"),
            BseEvent("920999", "其他股份"),
            new DateOnly(2026, 8, 19),
            new DateOnly(2026, 8, 25));

        Assert.AreEqual(0, parsed.References.Count);
    }

    [TestMethod]
    public async Task Bse_Provider_Uses_Detail_Api_Before_Disclosure_Fallback()
    {
        using var handler = new StubHttpMessageHandler((_, _) => Task.FromResult(
            StubHttpMessageHandler.Text(
                FixtureLoader.Read("Announcements/bse-detail-920289-20260824.jsonp"),
                "application/javascript")));
        using var client = new HttpClient(handler);
        var provider = new BseAnnouncementProvider(
            client,
            new RepositoryTestContext.MutableTimeProvider(new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero)));

        var references = await provider.SearchAsync(
            BseEvent(),
            new DateOnly(2026, 8, 19),
            new DateOnly(2026, 8, 25),
            CancellationToken.None);

        AssertExactlyOne(references);
        Assert.AreEqual(1, handler.Requests.Count);
        StringAssert.Contains(handler.Requests[0].Uri!.AbsolutePath, "infoDetailResult.do");
        StringAssert.Contains(handler.Requests[0].Uri.Query, "id=346");
    }

    [TestMethod]
    public async Task Bse_Provider_Falls_Back_To_Public_Disclosure_Search_When_Detail_Breaks()
    {
        using var handler = new StubHttpMessageHandler((request, _) => Task.FromResult(
            request.RequestUri!.AbsolutePath.Contains("infoDetailResult.do", StringComparison.Ordinal)
                ? StubHttpMessageHandler.Text("not-jsonp", "text/plain")
                : StubHttpMessageHandler.Text(
                    FixtureLoader.Read("Announcements/bse-disclosure-920289-20260824.jsonp"),
                    "application/javascript")));
        using var client = new HttpClient(handler);
        var provider = new BseAnnouncementProvider(
            client,
            new RepositoryTestContext.MutableTimeProvider(new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero)));

        var references = await provider.SearchAsync(
            BseEvent(),
            new DateOnly(2026, 8, 19),
            new DateOnly(2026, 8, 25),
            CancellationToken.None);

        var item = AssertExactlyOne(references);
        Assert.AreEqual("https://www.bseinfo.net/disclosure/2026/2026-08-20/1787216709992_052262.pdf", item.Url.ToString());
        Assert.AreEqual(2, handler.Requests.Count);
        StringAssert.Contains(handler.Requests[1].Uri!.AbsolutePath, "zoneInfoResult.do");
        StringAssert.Contains(Uri.UnescapeDataString(handler.Requests[1].Uri.Query), "disclosureTypes[]=9533");
        StringAssert.Contains(handler.Requests[1].Uri.Query, "companyCd=920289");
    }

    [TestMethod]
    public void Provider_Contract_Breaks_Are_Rejected_Instead_Of_Returning_Empty()
    {
        Assert.ThrowsExactly<JsonException>(() => SseAnnouncementProvider.Parse("{\"pageHelp\":{}}"));
        Assert.ThrowsExactly<JsonException>(() => CninfoAnnouncementProvider.Parse("{\"totalAnnouncement\":0}"));
        Assert.ThrowsExactly<JsonException>(() => BseAnnouncementProvider.ParsePage(
            "callback([{\"status\":0}])",
            BseEvent(),
            new DateOnly(2026, 8, 19),
            new DateOnly(2026, 8, 25)));
    }

    private static IpoEvent BseEvent(string securityCode = "920289", string name = "华汇智能") => new()
    {
        Id = $"beijing:{securityCode}",
        Exchange = Exchange.Beijing,
        Board = Board.Beijing,
        SecurityCode = securityCode,
        ApplyCode = securityCode,
        LegacyCode = securityCode == "920289" ? "874378" : null,
        Name = name,
        ApplyDate = new DateOnly(2026, 8, 24),
        Status = IssueStatus.Active,
        AnnouncementUrl = "https://www.bseinfo.net/newshare/listofissues_detail.html?id=346",
        FirstSeenAt = new DateTimeOffset(2026, 8, 20, 1, 0, 0, TimeSpan.Zero),
        UpdatedAt = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero),
    };

    private static IpoEvent ShenzhenEvent() => new()
    {
        Id = "shenzhen:301688",
        Exchange = Exchange.Shenzhen,
        Board = Board.ChiNext,
        SecurityCode = "301688",
        ApplyCode = "301688",
        Name = "格林生物",
        ApplyDate = new DateOnly(2026, 8, 25),
        Status = IssueStatus.Active,
        FirstSeenAt = new DateTimeOffset(2026, 8, 20, 1, 0, 0, TimeSpan.Zero),
        UpdatedAt = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero),
    };

    private static T AssertExactlyOne<T>(IReadOnlyList<T> values)
    {
        Assert.AreEqual(1, values.Count);
        return values[0];
    }
}
