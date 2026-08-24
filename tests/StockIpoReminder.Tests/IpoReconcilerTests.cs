using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class IpoReconcilerTests
{
    private readonly IpoReconciler _reconciler = new();
    private readonly AppSettings _settings = new();
    private readonly DateTimeOffset _now = new(2026, 8, 24, 10, 0, 0, TimeSpan.FromHours(8));

    [TestMethod]
    public void Higher_Priority_NonEmpty_Value_Wins_And_Empty_Does_Not_Overwrite()
    {
        var candidates = new[]
        {
            Candidate("eastmoney", 100, applyCode: "732448", issuePrice: 18.66m),
            Candidate("sse", 200, applyCode: null, issuePrice: null),
        };

        var result = _reconciler.Reconcile(candidates, null, _settings, _now)!;

        Assert.AreEqual("732448", result.Event.ApplyCode);
        Assert.AreEqual(18.66m, result.Event.IssuePrice);
    }

    [TestMethod]
    public void Conflicting_NonEmpty_Critical_Values_Are_Flagged()
    {
        var candidates = new[]
        {
            Candidate("eastmoney", 100, applyCode: "732448", issuePrice: 18.66m),
            Candidate("sse", 200, applyCode: "732449", issuePrice: 18.66m),
        };

        var result = _reconciler.Reconcile(candidates, null, _settings, _now)!;

        Assert.IsTrue(result.Event.DataConflict);
        Assert.AreEqual(DataQualityStatus.DataConflict, result.Event.DataQualityStatus);
        CollectionAssert.Contains(result.ConflictFields.ToList(), nameof(IpoCandidate.ApplyCode));
    }

    [TestMethod]
    public void Announcement_Has_Final_Priority()
    {
        var candidates = new[]
        {
            Candidate("eastmoney", 100, applyCode: "732448", issuePrice: 18.66m),
            Candidate("sse-announcement", 1000, applyCode: "732450", issuePrice: 19.01m, announcement: true),
        };

        var result = _reconciler.Reconcile(candidates, null, _settings, _now)!;

        Assert.AreEqual("732450", result.Event.ApplyCode);
        Assert.AreEqual(19.01m, result.Event.IssuePrice);
        Assert.AreEqual(DataQualityStatus.DataConflict, result.Event.DataQualityStatus, "A conflict remains visible even when announcement wins.");
    }

    [TestMethod]
    public void Known_Date_With_Missing_Apply_Code_Becomes_Manual_Review()
    {
        var candidate = Candidate("sse", 200, applyCode: null, issuePrice: null);

        var result = _reconciler.Reconcile([candidate], null, _settings, _now)!;

        Assert.AreEqual(DataQualityStatus.ManualReviewRequired, result.Event.DataQualityStatus);
        Assert.AreEqual(IpoLifecycleStatus.Scheduled, result.Event.LifecycleStatus);
    }

    [TestMethod]
    public void North_Market_Defaults_To_Full_Cash_Sessions()
    {
        var candidate = Candidate("bse", 200, applyCode: "920001", issuePrice: 12.3m) with
        {
            Exchange = Exchange.Beijing,
            Board = Board.Beijing,
            SecurityCode = "874001",
        };

        var result = _reconciler.Reconcile([candidate], null, _settings, _now)!;

        Assert.IsTrue(result.Event.Sessions.All(x => x.FundingMode == FundingMode.FullCash));
        Assert.IsTrue(result.Event.Sessions.All(x => x.AllocationTimeSensitive));
    }

    private static IpoCandidate Candidate(string source, int priority, string? applyCode, decimal? issuePrice, bool announcement = false) => new()
    {
        Source = source,
        SourcePriority = priority,
        FetchedAt = new DateTimeOffset(2026, 8, 24, 9, 0, 0, TimeSpan.FromHours(8)),
        Exchange = Exchange.Shanghai,
        Board = Board.Main,
        SecurityCode = "603448",
        ApplyCode = applyCode,
        Name = "天博智能",
        ApplyDate = new DateOnly(2026, 8, 26),
        IssuePrice = issuePrice,
        Status = IssueStatus.Upcoming,
        IsAnnouncementDerived = announcement,
    };
}
