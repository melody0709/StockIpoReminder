using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Announcements;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class AnnouncementParserTests
{
    private readonly AnnouncementFieldParser _parser = new();

    [TestMethod]
    public void Bse_Official_Excerpt_Parses_All_Reminder_Critical_Fields()
    {
        var text = FixtureLoader.Read("Announcements/bse-920289-excerpt-20260820.txt");

        var fields = _parser.Parse(text, "华汇智能向不特定合格投资者公开发行股票并在北京证券交易所上市发行公告");

        AssertField(fields, "SecurityCode", "920289", 0.98m);
        AssertField(fields, "ApplyCode", "920289", 0.99m);
        AssertField(fields, "ApplyDate", "2026-08-24", 0.98m);
        AssertField(fields, "IssuePrice", "17.71", 0.98m);
        AssertField(fields, "MaxApplyQuantity", "722500", 0.92m);
        AssertField(fields, "LotSize", "100", 0.95m);
        AssertField(fields, "OfficialSessions", "9:15-11:30,13:00-15:00", 0.95m);
        AssertField(fields, "FundingMode", "FullCash", 0.99m);
        Assert.IsFalse(fields.Any(field => field.Name == "IssueStatus"), "普通发行公告中的条件性风险提示不能改变发行状态。");
        Assert.IsTrue(fields.Where(field => field.Name != "IssueStatus").All(field => field.CharacterOffset is >= 0));
        Assert.IsTrue(fields.All(field => !string.IsNullOrWhiteSpace(field.Evidence)));

        var candidate = AnnouncementCandidateFactory.Create(
            CreateEvent(),
            Document(fields, "华汇智能向不特定合格投资者公开发行股票并在北京证券交易所上市发行公告"));
        Assert.IsTrue(candidate.Sessions.Count > 0);
        Assert.IsTrue(candidate.Sessions.All(static session => session.FundingMode == FundingMode.FullCash));
    }

    [TestMethod]
    public void Explicit_Status_Title_Wins_And_Maps_To_Domain_Status()
    {
        var expectations = new Dictionary<string, IssueStatus>
        {
            ["关于终止发行的公告"] = IssueStatus.Terminated,
            ["关于中止发行的公告"] = IssueStatus.Suspended,
            ["关于暂缓发行的公告"] = IssueStatus.Postponed,
            ["关于延期发行的公告"] = IssueStatus.Postponed,
            ["关于重新启动发行的公告"] = IssueStatus.Upcoming,
        };

        foreach (var pair in expectations)
        {
            var fields = _parser.Parse("正文", pair.Key);
            var status = fields.Single(field => field.Name == "IssueStatus");
            Assert.AreEqual(0.99m, status.Confidence);

            var candidate = AnnouncementCandidateFactory.Create(CreateEvent(), Document(fields, pair.Key));
            Assert.AreEqual(pair.Value, candidate.Status, pair.Key);
        }
    }

    [TestMethod]
    public void Bse_Security_Code_Used_For_Online_Subscription_Is_The_Apply_Code()
    {
        var fields = _parser.Parse(
            "投资者在申购时间内，按照发行价格，通过证券公司进行申购委托，使用证券代码“920289”进行网上申购。",
            "发行公告");

        AssertField(fields, "ApplyCode", "920289", 0.99m);
    }

    [TestMethod]
    public void Explicit_Body_Decision_Is_Recognized_But_Generic_Risk_Wording_Is_Not()
    {
        var explicitFields = _parser.Parse("经发行人与主承销商审慎研究，决定中止发行。", "情况说明");
        var riskFields = _parser.Parse("若有效申购不足，发行人和主承销商可能协商中止发行。", "发行公告");

        Assert.AreEqual("中止发行", explicitFields.Single(field => field.Name == "IssueStatus").Value);
        Assert.IsFalse(riskFields.Any(field => field.Name == "IssueStatus"));
    }

    [TestMethod]
    public void Low_Confidence_Fields_Do_Not_Override_Existing_Event()
    {
        var existing = CreateEvent();
        var document = Document(
        [
            new ParsedAnnouncementField
            {
                Name = "IssuePrice",
                Value = "99.99",
                Confidence = 0.50m,
                Evidence = "模糊扫描结果",
            },
        ],
        "发行公告");

        var candidate = AnnouncementCandidateFactory.Create(existing, document);

        Assert.IsNull(candidate.IssuePrice);
        Assert.AreEqual(existing.Status, candidate.Status);
        Assert.AreEqual(1, candidate.Fields.Count);
    }

    private static void AssertField(
        IReadOnlyList<ParsedAnnouncementField> fields,
        string name,
        string value,
        decimal confidence)
    {
        var field = fields.Single(item => item.Name == name);
        Assert.AreEqual(value, field.Value);
        Assert.AreEqual(confidence, field.Confidence);
    }

    private static IpoEvent CreateEvent() => new()
    {
        Id = "beijing:920289",
        Exchange = Exchange.Beijing,
        Board = Board.Beijing,
        SecurityCode = "920289",
        ApplyCode = "920289",
        LegacyCode = "874378",
        Name = "华汇智能",
        ApplyDate = new DateOnly(2026, 8, 24),
        IssuePrice = 17.71m,
        Status = IssueStatus.Upcoming,
        LifecycleStatus = IpoLifecycleStatus.Scheduled,
        EventVersion = 1,
        FirstSeenAt = new DateTimeOffset(2026, 8, 20, 1, 0, 0, TimeSpan.Zero),
        UpdatedAt = new DateTimeOffset(2026, 8, 20, 1, 0, 0, TimeSpan.Zero),
    };

    private static AnnouncementDocument Document(IReadOnlyList<ParsedAnnouncementField> fields, string title) => new()
    {
        Id = "fixture-document",
        IpoEventId = "beijing:920289",
        Reference = new AnnouncementReference
        {
            Provider = "bse-announcement",
            AnnouncementId = "fixture",
            Title = title,
            Url = new Uri("https://example.invalid/fixture.pdf"),
            PublishedAt = new DateTimeOffset(2026, 8, 20, 0, 0, 0, TimeSpan.FromHours(8)),
        },
        LocalPath = "fixture.pdf",
        FileHash = new string('a', 64),
        ExtractionStatus = ExtractionStatus.Extracted,
        ParserVersion = AnnouncementFieldParser.Version,
        ParsedFields = fields,
        DownloadedAt = new DateTimeOffset(2026, 8, 20, 2, 0, 0, TimeSpan.Zero),
    };
}
