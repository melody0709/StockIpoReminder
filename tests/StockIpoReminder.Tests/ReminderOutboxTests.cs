using Microsoft.Data.Sqlite;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class ReminderOutboxTests
{
    [TestMethod]
    public async Task Enqueue_Is_Idempotent_By_DedupeKey()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var ipo = RepositoryTestContext.Reconciled("shanghai:601001", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(ipo);
        var reminder = Reminder(ipo.Event.Id, 1, new DateOnly(2026, 8, 26), new TimeOnly(9, 25), "same-key");

        await context.Repository.EnqueueRemindersAsync([reminder, reminder]);
        await context.Repository.EnqueueRemindersAsync([reminder]);

        await using var connection = await context.OpenConnectionAsync();
        Assert.AreEqual(1L, await CountAsync(connection, "SELECT COUNT(*) FROM reminder_outbox;"));
    }

    [TestMethod]
    public async Task Claim_Collapses_Only_Selected_Events_When_Limit_Is_Smaller_Than_Due_Event_Count()
    {
        var now = ChinaTime.At(new DateOnly(2026, 8, 26), new TimeOnly(14, 30));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var first = RepositoryTestContext.Reconciled("shanghai:601002", new DateOnly(2026, 8, 26));
        var second = RepositoryTestContext.Reconciled("shanghai:601003", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(first);
        await context.Repository.UpsertEventAsync(second);
        await context.Repository.EnqueueRemindersAsync(
        [
            Reminder(first.Event.Id, 1, first.Event.ApplyDate!.Value, new TimeOnly(13, 0), "first-old"),
            Reminder(first.Event.Id, 1, first.Event.ApplyDate!.Value, new TimeOnly(14, 0), "first-new"),
            Reminder(second.Event.Id, 1, second.Event.ApplyDate!.Value, new TimeOnly(13, 0), "second-old"),
            Reminder(second.Event.Id, 1, second.Event.ApplyDate!.Value, new TimeOnly(14, 0), "second-new"),
        ]);

        var firstClaim = await context.Repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 1);
        var claimedEventId = AssertExactlyOne(firstClaim).Event.Id;
        var waitingEventId = claimedEventId == first.Event.Id ? second.Event.Id : first.Event.Id;

        await using (var connection = await context.OpenConnectionAsync())
        {
            Assert.AreEqual(1L, await CountStateAsync(connection, claimedEventId, ReminderDeliveryState.Leased));
            Assert.AreEqual(1L, await CountStateAsync(connection, claimedEventId, ReminderDeliveryState.Collapsed));
            Assert.AreEqual(2L, await CountStateAsync(connection, waitingEventId, ReminderDeliveryState.Pending));
        }

        var secondClaim = await context.Repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 1);
        Assert.AreEqual(waitingEventId, AssertExactlyOne(secondClaim).Event.Id);
    }

    [TestMethod]
    public async Task Expired_Lease_Can_Be_Reclaimed_And_Increments_Attempt_Count()
    {
        var now = ChinaTime.At(new DateOnly(2026, 8, 26), new TimeOnly(9, 30));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var ipo = RepositoryTestContext.Reconciled("shanghai:601004", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(ipo);
        await context.Repository.EnqueueRemindersAsync(
            [Reminder(ipo.Event.Id, 1, ipo.Event.ApplyDate!.Value, new TimeOnly(9, 25), "lease-key")]);

        var first = AssertExactlyOne(await context.Repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 20));
        Assert.AreEqual(1, first.AttemptCount);
        Assert.AreEqual(0, (await context.Repository.ClaimDueRemindersAsync(now.AddMinutes(1), TimeSpan.FromMinutes(2), 20)).Count);

        var reclaimed = AssertExactlyOne(await context.Repository.ClaimDueRemindersAsync(now.AddMinutes(3), TimeSpan.FromMinutes(2), 20));
        Assert.AreEqual(first.OutboxId, reclaimed.OutboxId);
        Assert.AreEqual(2, reclaimed.AttemptCount);
    }

    [TestMethod]
    public async Task Acknowledgement_Wins_Race_Against_Late_Fail_And_Complete()
    {
        var now = ChinaTime.At(new DateOnly(2026, 8, 26), new TimeOnly(9, 30));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var ipo = RepositoryTestContext.Reconciled("shanghai:601005", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(ipo);
        await context.Repository.EnqueueRemindersAsync(
            [Reminder(ipo.Event.Id, 1, ipo.Event.ApplyDate!.Value, new TimeOnly(9, 25), "race-key")]);
        var leased = AssertExactlyOne(await context.Repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 20));

        await context.Repository.AcknowledgeAsync(ipo.Event.Id, 1, now.AddSeconds(1), "ack-hash");
        await context.Repository.FailReminderAsync(leased.OutboxId, now.AddMinutes(1), "late sink error");
        await context.Repository.CompleteReminderAsync(leased.OutboxId, now.AddSeconds(2), "test");

        await using var connection = await context.OpenConnectionAsync();
        Assert.AreEqual((long)ReminderDeliveryState.Cancelled, await StateAsync(connection, leased.OutboxId));
        Assert.AreEqual(0L, await CountAsync(connection, "SELECT COUNT(*) FROM reminder_log;"));
        Assert.AreEqual(0, (await context.Repository.ClaimDueRemindersAsync(now.AddMinutes(5), TimeSpan.FromMinutes(2), 20)).Count);
    }

    [TestMethod]
    public async Task Failed_Delivery_Is_Retried_At_Requested_Time()
    {
        var now = ChinaTime.At(new DateOnly(2026, 8, 26), new TimeOnly(9, 30));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var ipo = RepositoryTestContext.Reconciled("shanghai:601006", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(ipo);
        await context.Repository.EnqueueRemindersAsync(
            [Reminder(ipo.Event.Id, 1, ipo.Event.ApplyDate!.Value, new TimeOnly(9, 25), "retry-key")]);
        var leased = AssertExactlyOne(await context.Repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 20));

        await context.Repository.FailReminderAsync(leased.OutboxId, now.AddMinutes(3), "temporary failure");

        Assert.AreEqual(0, (await context.Repository.ClaimDueRemindersAsync(now.AddMinutes(2), TimeSpan.FromMinutes(2), 20)).Count);
        var retried = AssertExactlyOne(await context.Repository.ClaimDueRemindersAsync(now.AddMinutes(3), TimeSpan.FromMinutes(2), 20));
        Assert.AreEqual(2, retried.AttemptCount);
    }

    [TestMethod]
    public async Task Reconcile_Cancels_Obsolete_Pending_Rows_But_Preserves_Delivered_History()
    {
        var date = new DateOnly(2026, 8, 26);
        var now = ChinaTime.At(date, new TimeOnly(9, 30));
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var ipo = RepositoryTestContext.Reconciled("shanghai:601007", date);
        await context.Repository.UpsertEventAsync(ipo);
        var delivered = Reminder(ipo.Event.Id, 1, date, new TimeOnly(9, 25), "delivered-key");
        var obsolete = Reminder(ipo.Event.Id, 1, date, new TimeOnly(14, 55), "obsolete-key");
        await context.Repository.EnqueueRemindersAsync([delivered, obsolete]);
        var leased = AssertExactlyOne(await context.Repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 20));
        await context.Repository.CompleteReminderAsync(leased.OutboxId, now, "test");
        var replacement = Reminder(ipo.Event.Id, 1, date, new TimeOnly(14, 45), "replacement-key");

        await context.Repository.ReconcileReminderScheduleAsync(ipo.Event.Id, 1, [replacement], now.AddMinutes(1));

        await using var connection = await context.OpenConnectionAsync();
        Assert.AreEqual((long)ReminderDeliveryState.Delivered, await StateByKeyAsync(connection, delivered.DedupeKey));
        Assert.AreEqual((long)ReminderDeliveryState.Cancelled, await StateByKeyAsync(connection, obsolete.DedupeKey));
        Assert.AreEqual((long)ReminderDeliveryState.Pending, await StateByKeyAsync(connection, replacement.DedupeKey));
    }

    private static ReminderScheduleItem Reminder(
        string eventId,
        int eventVersion,
        DateOnly date,
        TimeOnly time,
        string key,
        ReminderLevel level = ReminderLevel.Hourly) => new()
    {
        IpoEventId = eventId,
        EventVersion = eventVersion,
        DueAt = ChinaTime.At(date, time),
        Level = level,
        DedupeKey = key,
    };

    private static T AssertExactlyOne<T>(IReadOnlyList<T> values)
    {
        Assert.AreEqual(1, values.Count);
        return values[0];
    }

    private static async Task<long> CountStateAsync(SqliteConnection connection, string eventId, ReminderDeliveryState state)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id = $id AND delivery_state = $state;";
        command.Parameters.AddWithValue("$id", eventId);
        command.Parameters.AddWithValue("$state", (int)state);
        return Convert.ToInt64(await command.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture);
    }

    private static async Task<long> StateAsync(SqliteConnection connection, long id)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT delivery_state FROM reminder_outbox WHERE id = $id;";
        command.Parameters.AddWithValue("$id", id);
        return Convert.ToInt64(await command.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture);
    }

    private static async Task<long> StateByKeyAsync(SqliteConnection connection, string key)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT delivery_state FROM reminder_outbox WHERE dedupe_key = $key;";
        command.Parameters.AddWithValue("$key", key);
        return Convert.ToInt64(await command.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture);
    }

    private static async Task<long> CountAsync(SqliteConnection connection, string sql)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        return Convert.ToInt64(await command.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture);
    }
}
