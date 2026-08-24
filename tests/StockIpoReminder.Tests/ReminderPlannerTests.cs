using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class ReminderPlannerTests
{
    private readonly ReminderPlanner _planner = new();
    private readonly AppSettings _settings = new();

    [TestMethod]
    public void Shanghai_Uses_0925_PreOpen_And_Escalates_To_Default_1455_Cutoff()
    {
        var ipo = CreateEvent(Exchange.Shanghai, new DateOnly(2026, 8, 26));

        var result = _planner.Plan(ipo, _settings);

        AssertHas(result, new TimeOnly(9, 25), ReminderLevel.MarketOpening);
        AssertHas(result, new TimeOnly(11, 20), ReminderLevel.NoonBoundary);
        AssertHas(result, new TimeOnly(12, 55), ReminderLevel.AfternoonOpening);
        AssertHas(result, new TimeOnly(13, 55), ReminderLevel.FifteenMinutes);
        AssertHas(result, new TimeOnly(14, 25), ReminderLevel.FiveMinutes);
        AssertHas(result, new TimeOnly(14, 45), ReminderLevel.TwoMinutes);
        AssertHas(result, new TimeOnly(14, 55), ReminderLevel.Final);
    }

    [TestMethod]
    public void Explicit_1500_Cutoff_Uses_1400_1430_1450_Boundaries()
    {
        var settings = _settings with { SafetyCutoff = new TimeOnly(15, 0) };
        var ipo = CreateEvent(Exchange.Shanghai, new DateOnly(2026, 8, 26));

        var result = _planner.Plan(ipo, settings);

        AssertHas(result, new TimeOnly(14, 0), ReminderLevel.FifteenMinutes);
        AssertHas(result, new TimeOnly(14, 30), ReminderLevel.FiveMinutes);
        AssertHas(result, new TimeOnly(14, 50), ReminderLevel.TwoMinutes);
        AssertHas(result, new TimeOnly(15, 0), ReminderLevel.Final);
    }

    [TestMethod]
    public void Shenzhen_Uses_0910_PreOpen()
    {
        var ipo = CreateEvent(Exchange.Shenzhen, new DateOnly(2026, 8, 26));

        var result = _planner.Plan(ipo, _settings);

        AssertHas(result, new TimeOnly(9, 10), ReminderLevel.MarketOpening);
    }

    [TestMethod]
    public void Beijing_Adds_Early_Broker_Reminder_When_Configured()
    {
        var settings = _settings with
        {
            BeijingBrokerAcceptStart = new TimeOnly(8, 40),
            BeijingReservationSupported = true,
        };
        var ipo = CreateEvent(Exchange.Beijing, new DateOnly(2026, 8, 26));

        var result = _planner.Plan(ipo, settings);

        AssertHas(result, new TimeOnly(8, 40), ReminderLevel.BrokerOpening);
        AssertHas(result, new TimeOnly(9, 10), ReminderLevel.MarketOpening);
    }

    [TestMethod]
    public void Beijing_Does_Not_Add_Early_Broker_Reminder_Without_Reservation_Support()
    {
        var settings = _settings with
        {
            BeijingBrokerAcceptStart = new TimeOnly(8, 40),
            BeijingReservationSupported = false,
        };
        var ipo = CreateEvent(Exchange.Beijing, new DateOnly(2026, 8, 26));

        var result = _planner.Plan(ipo, settings);

        Assert.IsFalse(result.Any(x => x.Level == ReminderLevel.BrokerOpening));
        AssertHas(result, new TimeOnly(9, 10), ReminderLevel.MarketOpening);
    }

    [TestMethod]
    public void Earlier_Safety_Cutoff_Shifts_Escalation()
    {
        var settings = _settings with { SafetyCutoff = new TimeOnly(14, 45) };
        var ipo = CreateEvent(Exchange.Shanghai, new DateOnly(2026, 8, 26));

        var result = _planner.Plan(ipo, settings);

        AssertHas(result, new TimeOnly(13, 45), ReminderLevel.FifteenMinutes);
        AssertHas(result, new TimeOnly(14, 15), ReminderLevel.FiveMinutes);
        AssertHas(result, new TimeOnly(14, 35), ReminderLevel.TwoMinutes);
        AssertHas(result, new TimeOnly(14, 45), ReminderLevel.Final);
        Assert.IsFalse(result.Any(x =>
            DateOnly.FromDateTime(x.DueAt.DateTime) == new DateOnly(2026, 8, 26)
            && TimeOnly.FromDateTime(x.DueAt.DateTime) > new TimeOnly(14, 45)));
    }

    [TestMethod]
    public void Effective_Cutoff_Uses_Current_Setting_And_Is_Clamped_To_Official_End()
    {
        var ipo = CreateEvent(Exchange.Shanghai, new DateOnly(2026, 8, 26)) with
        {
            Sessions =
            [
                new SubscriptionSession
                {
                    SessionNumber = 1,
                    OfficialStart = new TimeOnly(9, 30),
                    OfficialEnd = new TimeOnly(11, 30),
                    SafetyCutoff = new TimeOnly(14, 10),
                },
                new SubscriptionSession
                {
                    SessionNumber = 2,
                    OfficialStart = new TimeOnly(13, 0),
                    OfficialEnd = new TimeOnly(14, 45),
                    SafetyCutoff = new TimeOnly(14, 10),
                },
            ],
        };

        Assert.AreEqual(
            new TimeOnly(14, 30),
            ReminderPlanner.GetEffectiveSafetyCutoff(ipo, _settings with { SafetyCutoff = new TimeOnly(14, 30) }));
        Assert.AreEqual(
            new TimeOnly(14, 45),
            ReminderPlanner.GetEffectiveSafetyCutoff(ipo, _settings with { SafetyCutoff = new TimeOnly(15, 0) }));
    }

    [TestMethod]
    public void Terminal_Event_Has_No_Reminders()
    {
        var ipo = CreateEvent(Exchange.Shanghai, new DateOnly(2026, 8, 26)) with
        {
            Status = IssueStatus.Terminated,
            LifecycleStatus = IpoLifecycleStatus.SuspendedOrCancelled,
        };

        Assert.AreEqual(0, _planner.Plan(ipo, _settings).Count);
    }

    [TestMethod]
    public void Acknowledged_Event_Has_No_Reminders_But_NeedsReview_Does()
    {
        var acknowledged = CreateEvent(Exchange.Shanghai, new DateOnly(2026, 8, 26)) with
        {
            LifecycleStatus = IpoLifecycleStatus.Acknowledged,
        };
        var needsReview = acknowledged with { LifecycleStatus = IpoLifecycleStatus.AcknowledgedNeedsReview };

        Assert.AreEqual(0, _planner.Plan(acknowledged, _settings).Count);
        Assert.IsGreaterThan(0, _planner.Plan(needsReview, _settings).Count);
    }

    [TestMethod]
    public void Disabled_Market_Has_No_Reminders()
    {
        var settings = _settings with { BeijingEnabled = false };
        var ipo = CreateEvent(Exchange.Beijing, new DateOnly(2026, 8, 26));

        Assert.AreEqual(0, _planner.Plan(ipo, settings).Count);
    }

    private static IpoEvent CreateEvent(Exchange exchange, DateOnly date) => new()
    {
        Id = $"test:{exchange}",
        Exchange = exchange,
        Board = exchange == Exchange.Beijing ? Board.Beijing : Board.Main,
        SecurityCode = "600001",
        ApplyCode = "700001",
        Name = "测试股份",
        ApplyDate = date,
        Status = IssueStatus.Upcoming,
        LifecycleStatus = IpoLifecycleStatus.Scheduled,
        FirstSeenAt = DateTimeOffset.UtcNow,
        UpdatedAt = DateTimeOffset.UtcNow,
    };

    private static void AssertHas(IEnumerable<ReminderScheduleItem> result, TimeOnly time, ReminderLevel level) =>
        Assert.IsTrue(result.Any(x => TimeOnly.FromDateTime(x.DueAt.DateTime) == time && x.Level == level), $"Missing {time} {level}");
}
