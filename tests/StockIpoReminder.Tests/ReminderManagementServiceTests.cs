using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class ReminderManagementServiceTests
{
    [TestMethod]
    public async Task Acknowledge_Cancels_All_Pending_Reminders_For_That_Event_Version()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(10, 0));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await SeedActiveEventAsync(context, "shanghai:601101", date);
        var planner = new ReminderPlanner();
        await context.Repository.ReconcileReminderScheduleAsync(
            seeded.Id,
            seeded.EventVersion,
            planner.Plan(seeded, new AppSettings()),
            now);
        var service = new ReminderManagementService(context.Repository, planner, new RecordingSyncTrigger(), context.TimeProvider);

        await service.AcknowledgeAsync(seeded.Id, seeded.EventVersion);

        var acknowledged = await context.Repository.GetEventAsync(seeded.Id);
        Assert.IsNotNull(acknowledged);
        Assert.AreEqual(IpoLifecycleStatus.Acknowledged, acknowledged.LifecycleStatus);
        Assert.AreEqual(0L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id = 'shanghai:601101' AND delivery_state IN (0, 1);"));
        Assert.AreEqual(1L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM acknowledgements WHERE ipo_event_id = 'shanghai:601101' AND event_version = 1 AND revoked_at IS NULL;"));
    }

    [TestMethod]
    public async Task Revoke_Before_Cutoff_Restores_Only_Not_Yet_Due_Reminders()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(10, 0));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await SeedActiveEventAsync(context, "shanghai:601102", date);
        var planner = new ReminderPlanner();
        var service = new ReminderManagementService(context.Repository, planner, new RecordingSyncTrigger(), context.TimeProvider);
        await service.AcknowledgeAsync(seeded.Id, seeded.EventVersion);

        await service.RevokeAcknowledgementAsync(seeded.Id, seeded.EventVersion);

        var restored = await context.Repository.GetEventAsync(seeded.Id);
        Assert.IsNotNull(restored);
        Assert.AreEqual(IpoLifecycleStatus.ActiveUnconfirmed, restored.LifecycleStatus);
        Assert.IsGreaterThan(0L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id = 'shanghai:601102' AND delivery_state = 0;"));
        var earliest = await ScalarAsync<string>(context, "SELECT MIN(due_at) FROM reminder_outbox WHERE ipo_event_id = 'shanghai:601102' AND delivery_state = 0;");
        Assert.IsGreaterThanOrEqualTo(now.UtcDateTime, DateTimeOffset.Parse(earliest, System.Globalization.CultureInfo.InvariantCulture).UtcDateTime);
    }

    [TestMethod]
    public async Task Revoke_With_Stale_Event_Version_Is_Rejected()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(10, 0));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await SeedActiveEventAsync(context, "shanghai:601103", date);
        await context.Repository.AcknowledgeAsync(seeded.Id, seeded.EventVersion, now, EventDataHasher.Compute(seeded));
        await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            seeded.Id,
            date,
            applyCode: "730103",
            issuePrice: 12.25m,
            lifecycle: IpoLifecycleStatus.Acknowledged,
            status: IssueStatus.Active,
            sessions: seeded.Sessions));
        var service = new ReminderManagementService(
            context.Repository,
            new ReminderPlanner(),
            new RecordingSyncTrigger(),
            context.TimeProvider);

        await Assert.ThrowsExactlyAsync<InvalidOperationException>(
            () => service.RevokeAcknowledgementAsync(seeded.Id, seeded.EventVersion));

        var current = await context.Repository.GetEventAsync(seeded.Id);
        Assert.IsNotNull(current);
        Assert.AreEqual(2, current.EventVersion);
        Assert.AreEqual(IpoLifecycleStatus.AcknowledgedNeedsReview, current.LifecycleStatus);
    }

    [TestMethod]
    public async Task Revoke_After_Safety_Cutoff_Is_Rejected()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(14, 56));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await SeedActiveEventAsync(context, "shanghai:601104", date);
        await context.Repository.AcknowledgeAsync(seeded.Id, seeded.EventVersion, now.AddMinutes(-2), EventDataHasher.Compute(seeded));
        var service = new ReminderManagementService(
            context.Repository,
            new ReminderPlanner(),
            new RecordingSyncTrigger(),
            context.TimeProvider);

        await Assert.ThrowsExactlyAsync<InvalidOperationException>(
            () => service.RevokeAcknowledgementAsync(seeded.Id, seeded.EventVersion));

        Assert.AreEqual(IpoLifecycleStatus.Acknowledged, (await context.Repository.GetEventAsync(seeded.Id))!.LifecycleStatus);
    }

    [TestMethod]
    public async Task Saving_Settings_Replans_Existing_Events_And_Requests_Synchronization()
    {
        var date = new DateOnly(2026, 8, 26);
        var now = ChinaTime.At(new DateOnly(2026, 8, 24), new TimeOnly(10, 0));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await SeedActiveEventAsync(context, "shanghai:601105", date, IpoLifecycleStatus.Scheduled);
        var trigger = new RecordingSyncTrigger();
        var service = new ReminderManagementService(context.Repository, new ReminderPlanner(), trigger, context.TimeProvider);

        await service.SaveSettingsAsync(new AppSettings { SafetyCutoff = new TimeOnly(14, 30) });

        Assert.AreEqual("设置变更", trigger.LastReason);
        var finalDue = await ScalarAsync<string>(context, "SELECT due_at FROM reminder_outbox WHERE ipo_event_id = 'shanghai:601105' AND delivery_state = 0 AND reminder_level = 80;");
        Assert.AreEqual(ChinaTime.At(date, new TimeOnly(14, 30)).UtcDateTime, DateTimeOffset.Parse(finalDue, System.Globalization.CultureInfo.InvariantCulture).UtcDateTime);
    }

    [TestMethod]
    public async Task Health_Summary_Separates_Healthy_Empty_From_Runtime_Failure()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(8, 0));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        await context.Repository.SaveCollectorResultAsync(new CollectorResult
        {
            Source = "official-source",
            Success = true,
            StartedAt = now,
            FinishedAt = now,
            RecordCount = 0,
            RawPayload = "[]",
            RawHash = "empty-hash",
            SchemaFingerprint = "empty-schema",
        });
        await context.Repository.TouchHeartbeatAsync("scheduler", now);
        await context.Repository.TouchHeartbeatAsync("delivery", now);
        var service = new ReminderManagementService(
            context.Repository,
            new ReminderPlanner(),
            new RecordingSyncTrigger(),
            context.TimeProvider);

        var healthyEmpty = await service.GetHealthSummaryAsync();
        Assert.AreEqual(HealthState.Healthy, healthyEmpty.OverallState);
        Assert.AreEqual(0, healthyEmpty.TodayTaskCount);

        context.TimeProvider.SetUtcNow(now.AddMinutes(4));
        var stalled = await service.GetHealthSummaryAsync();
        Assert.AreEqual(HealthState.Failed, stalled.OverallState);
        Assert.AreEqual(0, stalled.TodayTaskCount);
    }

    [TestMethod]
    public async Task Health_Summary_Is_Warning_When_Today_Has_Manual_Review_Task()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = ChinaTime.At(date, new TimeOnly(8, 0));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var reconciled = RepositoryTestContext.Reconciled(
            "shanghai:601106",
            date,
            lifecycle: IpoLifecycleStatus.ActiveUnconfirmed,
            status: IssueStatus.Active);
        await context.Repository.UpsertEventAsync(reconciled with
        {
            Event = reconciled.Event with { DataQualityStatus = DataQualityStatus.ManualReviewRequired },
        });
        await context.Repository.SaveCollectorResultAsync(new CollectorResult
        {
            Source = "official-source",
            Success = true,
            StartedAt = now,
            FinishedAt = now,
            RecordCount = 1,
            RawHash = "manual-review-hash",
            SchemaFingerprint = "manual-review-schema",
        });
        await context.Repository.TouchHeartbeatAsync("scheduler", now);
        await context.Repository.TouchHeartbeatAsync("delivery", now);
        var service = new ReminderManagementService(
            context.Repository,
            new ReminderPlanner(),
            new RecordingSyncTrigger(),
            context.TimeProvider);

        var summary = await service.GetHealthSummaryAsync();

        Assert.AreEqual(HealthState.Warning, summary.OverallState);
        Assert.AreEqual(1, summary.ManualReviewCount);
    }

    private static async Task<IpoEvent> SeedActiveEventAsync(
        RepositoryTestContext context,
        string id,
        DateOnly date,
        IpoLifecycleStatus lifecycle = IpoLifecycleStatus.ActiveUnconfirmed)
    {
        var settings = new AppSettings();
        var result = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            id,
            date,
            applyCode: $"7{id[^5..]}",
            lifecycle: lifecycle,
            status: IssueStatus.Active,
            sessions: MarketSessionFactory.CreateDefault(Exchange.Shanghai, settings)));
        return result.Event;
    }

    private static async Task<T> ScalarAsync<T>(RepositoryTestContext context, string sql)
    {
        await using var connection = await context.OpenConnectionAsync();
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        var value = await command.ExecuteScalarAsync();
        return (T)Convert.ChangeType(value!, typeof(T), System.Globalization.CultureInfo.InvariantCulture);
    }

    private sealed class RecordingSyncTrigger : ISyncTrigger
    {
        public string? LastReason { get; private set; }
        public void RequestSync(string reason) => LastReason = reason;
    }
}
