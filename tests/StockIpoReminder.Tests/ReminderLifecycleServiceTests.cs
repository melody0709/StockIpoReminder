using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class ReminderLifecycleServiceTests
{
    [TestMethod]
    public async Task Refresh_Uses_Current_Settings_Instead_Of_Stored_Session_Cutoff()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(14, 40));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        await context.Repository.SaveSettingsAsync(new AppSettings { SafetyCutoff = new TimeOnly(14, 55) });
        var seeded = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:601106",
            date,
            lifecycle: IpoLifecycleStatus.ActiveUnconfirmed,
            status: IssueStatus.Active,
            sessions: CreateSessions(new TimeOnly(14, 30))));
        var service = new ReminderLifecycleService(context.Repository);

        await service.RefreshAsync(now);

        Assert.AreEqual(
            IpoLifecycleStatus.ActiveUnconfirmed,
            (await context.Repository.GetEventAsync(seeded.Event.Id))!.LifecycleStatus);

        await context.Repository.SaveSettingsAsync(new AppSettings { SafetyCutoff = new TimeOnly(14, 35) });
        await service.RefreshAsync(now);

        Assert.AreEqual(
            IpoLifecycleStatus.ExpiredUnconfirmed,
            (await context.Repository.GetEventAsync(seeded.Event.Id))!.LifecycleStatus);
    }

    [TestMethod]
    public async Task Scheduled_Event_After_Current_Cutoff_Expires_In_One_Refresh()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(14, 56));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:601107",
            date,
            lifecycle: IpoLifecycleStatus.Scheduled,
            status: IssueStatus.Active,
            sessions: CreateSessions(new TimeOnly(14, 55))));
        var service = new ReminderLifecycleService(context.Repository);

        await service.RefreshAsync(now);

        Assert.AreEqual(
            IpoLifecycleStatus.ExpiredUnconfirmed,
            (await context.Repository.GetEventAsync(seeded.Event.Id))!.LifecycleStatus);
    }

    private static IReadOnlyList<SubscriptionSession> CreateSessions(TimeOnly storedCutoff) =>
    [
        new SubscriptionSession
        {
            SessionNumber = 1,
            OfficialStart = new TimeOnly(9, 30),
            OfficialEnd = new TimeOnly(11, 30),
            SafetyCutoff = storedCutoff,
            Source = "fixture",
        },
        new SubscriptionSession
        {
            SessionNumber = 2,
            OfficialStart = new TimeOnly(13, 0),
            OfficialEnd = new TimeOnly(15, 0),
            SafetyCutoff = storedCutoff,
            Source = "fixture",
        },
    ];
}
