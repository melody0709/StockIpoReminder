using Microsoft.Data.Sqlite;
using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class SqliteIpoRepositoryTests
{
    [TestMethod]
    public async Task Initialize_Is_Idempotent_And_Creates_Wal_Schema()
    {
        await using var context = await RepositoryTestContext.CreateAsync();

        await context.Repository.InitializeAsync();

        await using var connection = await context.OpenConnectionAsync();
        Assert.AreEqual("wal", await ScalarStringAsync(connection, "PRAGMA journal_mode;"));
        Assert.AreEqual(1L, await ScalarInt64Async(connection, "SELECT COUNT(*) FROM schema_migrations WHERE version = 1;"));
        Assert.AreEqual(1L, await ScalarInt64Async(connection, "SELECT COUNT(*) FROM schema_migrations WHERE version = 2;"));
        Assert.AreEqual(1L, await ScalarInt64Async(connection, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'reminder_outbox';"));
        Assert.AreEqual(1L, await ScalarInt64Async(connection, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'manual_overrides';"));
        Assert.AreEqual(1L, await ScalarInt64Async(connection, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'source_backoff';"));
    }

    [TestMethod]
    public async Task Partial_Update_Preserves_Previously_Known_Values_And_Sources()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initial = RepositoryTestContext.Reconciled("shanghai:600001", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(initial);

        var partial = new ReconciledIpoEvent
        {
            Event = initial.Event with
            {
                ApplyCode = null,
                LegacyCode = null,
                IssuePrice = null,
                LotSize = null,
                MaxApplyQuantity = null,
                RequiredMarketValue = null,
                BallotDate = null,
                PaymentDate = null,
                AnnouncementUrl = null,
                UpdatedAt = initial.Event.UpdatedAt.AddMinutes(30),
            },
            FieldSources = [],
        };

        var result = await context.Repository.UpsertEventAsync(partial);
        var stored = await context.Repository.GetEventAsync(initial.Event.Id);
        var sources = await context.Repository.GetFieldSourcesAsync(initial.Event.Id);

        Assert.IsNotNull(stored);
        Assert.AreEqual(initial.Event.ApplyCode, stored.ApplyCode);
        Assert.AreEqual(initial.Event.LegacyCode, stored.LegacyCode);
        Assert.AreEqual(initial.Event.IssuePrice, stored.IssuePrice);
        Assert.AreEqual(initial.Event.LotSize, stored.LotSize);
        Assert.AreEqual(initial.Event.MaxApplyQuantity, stored.MaxApplyQuantity);
        Assert.AreEqual(initial.Event.RequiredMarketValue, stored.RequiredMarketValue);
        Assert.AreEqual(initial.Event.BallotDate, stored.BallotDate);
        Assert.AreEqual(initial.Event.PaymentDate, stored.PaymentDate);
        Assert.AreEqual(initial.Event.AnnouncementUrl, stored.AnnouncementUrl);
        Assert.IsFalse(result.CriticalFieldsChanged);
        Assert.AreEqual(1, result.Event.EventVersion);
        Assert.AreEqual(1, sources.Count);
    }

    [TestMethod]
    public async Task Noncritical_Update_Does_Not_Lose_Acknowledged_State()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initial = RepositoryTestContext.Reconciled("shanghai:600002", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(initial);
        await context.Repository.AcknowledgeAsync(initial.Event.Id, 1, context.TimeProvider.GetUtcNow(), "hash-v1");

        var update = initial with
        {
            Event = initial.Event with
            {
                ListingDate = new DateOnly(2026, 9, 10),
                LifecycleStatus = IpoLifecycleStatus.Scheduled,
                UpdatedAt = initial.Event.UpdatedAt.AddHours(1),
            },
        };
        var result = await context.Repository.UpsertEventAsync(update);
        var stored = await context.Repository.GetEventAsync(initial.Event.Id);

        Assert.IsNotNull(stored);
        Assert.AreEqual(1, result.Event.EventVersion);
        Assert.AreEqual(IpoLifecycleStatus.Acknowledged, stored.LifecycleStatus);
        Assert.AreEqual(new DateOnly(2026, 9, 10), stored.ListingDate);
    }

    [TestMethod]
    public async Task Critical_Update_After_Acknowledgement_Creates_New_Version_And_Needs_Review()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initial = RepositoryTestContext.Reconciled("shanghai:600003", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(initial);
        await context.Repository.AcknowledgeAsync(initial.Event.Id, 1, context.TimeProvider.GetUtcNow(), "hash-v1");

        var update = initial with
        {
            Event = initial.Event with
            {
                IssuePrice = 11.25m,
                LifecycleStatus = IpoLifecycleStatus.Scheduled,
                UpdatedAt = initial.Event.UpdatedAt.AddHours(1),
            },
        };
        var result = await context.Repository.UpsertEventAsync(update);
        var stored = await context.Repository.GetEventAsync(initial.Event.Id);

        Assert.IsTrue(result.CriticalFieldsChanged);
        Assert.IsTrue(result.EventVersionChanged);
        Assert.AreEqual(2, result.Event.EventVersion);
        Assert.IsNotNull(stored);
        Assert.AreEqual(2, stored.EventVersion);
        Assert.AreEqual(IpoLifecycleStatus.AcknowledgedNeedsReview, stored.LifecycleStatus);
        Assert.AreEqual(11.25m, stored.IssuePrice);
    }

    [TestMethod]
    public async Task ApplyDate_Override_Moves_Effective_Event_Without_Changing_Public_Row()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initialDate = new DateOnly(2026, 8, 26);
        var overrideDate = new DateOnly(2026, 8, 27);
        var initial = RepositoryTestContext.Reconciled("shanghai:600004", initialDate);
        await context.Repository.UpsertEventAsync(initial);

        await context.Repository.AddManualOverrideAsync(
            initial.Event.Id,
            1,
            "ApplyDate",
            overrideDate.ToString("yyyy-MM-dd", System.Globalization.CultureInfo.InvariantCulture),
            "核对正式公告",
            null);

        var effective = await context.Repository.GetEventAsync(initial.Event.Id);
        var oldDateResults = await context.Repository.GetEventsAsync(initialDate, initialDate);
        var newDateResults = await context.Repository.GetEventsAsync(overrideDate, overrideDate);
        await using var connection = await context.OpenConnectionAsync();
        await using var rawDate = connection.CreateCommand();
        rawDate.CommandText = "SELECT apply_date FROM ipo_events WHERE id = $id;";
        rawDate.Parameters.AddWithValue("$id", initial.Event.Id);

        Assert.IsNotNull(effective);
        Assert.AreEqual(overrideDate, effective.ApplyDate);
        Assert.IsTrue(effective.HasManualOverride);
        CollectionAssert.Contains(effective.ManualOverrideFields.ToList(), "ApplyDate");
        Assert.AreEqual(0, oldDateResults.Count);
        Assert.AreEqual(1, newDateResults.Count);
        Assert.AreEqual("2026-08-26", Convert.ToString(await rawDate.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture));
    }

    [TestMethod]
    public async Task Revoking_Override_Restores_Public_Value()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initial = RepositoryTestContext.Reconciled("shanghai:600005", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(initial);
        await context.Repository.AddManualOverrideAsync(initial.Event.Id, 1, "IssuePrice", "99.90", "人工核验", null);
        var entry = AssertExactlyOne(await context.Repository.GetManualOverridesAsync(initial.Event.Id, 1));

        await context.Repository.RevokeManualOverrideAsync(entry.Id, context.TimeProvider.GetUtcNow().AddMinutes(1));

        var restored = await context.Repository.GetEventAsync(initial.Event.Id);
        Assert.IsNotNull(restored);
        Assert.AreEqual(initial.Event.IssuePrice, restored.IssuePrice);
        Assert.IsFalse(restored.HasManualOverride);
        Assert.AreEqual(0, restored.ManualOverrideFields.Count);
    }

    [TestMethod]
    public async Task External_Critical_Version_Change_Does_Not_Reuse_Old_Override()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initial = RepositoryTestContext.Reconciled("shanghai:600006", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(initial);
        await context.Repository.AddManualOverrideAsync(initial.Event.Id, 1, "IssuePrice", "99.90", "人工核验", null);

        var update = initial with
        {
            Event = initial.Event with { IssuePrice = 11.25m, UpdatedAt = initial.Event.UpdatedAt.AddHours(1) },
        };
        await context.Repository.UpsertEventAsync(update);

        var effective = await context.Repository.GetEventAsync(initial.Event.Id);
        Assert.IsNotNull(effective);
        Assert.AreEqual(2, effective.EventVersion);
        Assert.AreEqual(11.25m, effective.IssuePrice);
        Assert.IsFalse(effective.HasManualOverride);
        Assert.AreEqual(0, effective.ManualOverrideFields.Count);
        Assert.AreEqual(1, (await context.Repository.GetManualOverridesAsync(initial.Event.Id, 1)).Count);
        Assert.AreEqual(0, (await context.Repository.GetManualOverridesAsync(initial.Event.Id, 2)).Count);
    }

    [TestMethod]
    public async Task Manual_Override_On_Acknowledged_Event_Requires_Reconfirmation()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var initial = RepositoryTestContext.Reconciled("shanghai:600007", new DateOnly(2026, 8, 26));
        await context.Repository.UpsertEventAsync(initial);
        await context.Repository.AcknowledgeAsync(initial.Event.Id, 1, context.TimeProvider.GetUtcNow(), "hash-v1");

        await context.Repository.AddManualOverrideAsync(initial.Event.Id, 1, "MaxApplyQuantity", "20000", "公告更正", null);

        var effective = await context.Repository.GetEventAsync(initial.Event.Id);
        Assert.IsNotNull(effective);
        Assert.AreEqual(20000, effective.MaxApplyQuantity);
        Assert.AreEqual(IpoLifecycleStatus.AcknowledgedNeedsReview, effective.LifecycleStatus);
    }

    private static T AssertExactlyOne<T>(IReadOnlyList<T> values)
    {
        Assert.AreEqual(1, values.Count);
        return values[0];
    }

    private static async Task<long> ScalarInt64Async(SqliteConnection connection, string sql)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        return Convert.ToInt64(await command.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture);
    }

    private static async Task<string> ScalarStringAsync(SqliteConnection connection, string sql)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        return Convert.ToString(await command.ExecuteScalarAsync(), System.Globalization.CultureInfo.InvariantCulture) ?? string.Empty;
    }
}
