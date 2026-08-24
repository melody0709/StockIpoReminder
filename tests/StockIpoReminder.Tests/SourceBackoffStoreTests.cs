using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class SourceBackoffStoreTests
{
    [TestMethod]
    public async Task Failures_Use_Expected_Sequence_And_Cap_At_Thirty_Minutes()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var store = CreateStore(context);
        var now = context.TimeProvider.GetUtcNow();
        var expectedMinutes = new[] { 1, 2, 4, 8, 15, 30, 30 };

        foreach (var minutes in expectedMinutes)
        {
            var nextAttempt = await store.RecordFailureAsync(
                "fixture-source",
                now,
                retryAfter: null,
                error: "fixture failure");

            Assert.AreEqual(now.AddMinutes(minutes), nextAttempt);
        }

        var decision = await store.GetDecisionAsync("fixture-source", now);
        Assert.AreEqual(expectedMinutes.Length, decision.FailureCount);
        Assert.IsFalse(decision.CanAttempt);
        Assert.AreEqual(now.AddMinutes(30), decision.NextAttemptAt);
    }

    [TestMethod]
    public async Task Backoff_Blocks_Until_Deadline_And_Survives_Store_Recreation()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var now = context.TimeProvider.GetUtcNow();
        var firstStore = CreateStore(context);
        var nextAttempt = await firstStore.RecordFailureAsync(
            "persistent-source",
            now,
            retryAfter: null,
            error: "fixture failure");

        var recreatedStore = CreateStore(context);
        var before = await recreatedStore.GetDecisionAsync("persistent-source", nextAttempt.AddTicks(-1));
        var atDeadline = await recreatedStore.GetDecisionAsync("persistent-source", nextAttempt);

        Assert.IsFalse(before.CanAttempt);
        Assert.AreEqual(1, before.FailureCount);
        Assert.AreEqual(nextAttempt, before.NextAttemptAt);
        Assert.IsTrue(atDeadline.CanAttempt);
    }

    [TestMethod]
    public async Task Longer_RetryAfter_Wins_Over_Local_Backoff()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var store = CreateStore(context);
        var now = context.TimeProvider.GetUtcNow();

        var nextAttempt = await store.RecordFailureAsync(
            "rate-limited-source",
            now,
            retryAfter: TimeSpan.FromMinutes(10),
            error: "HTTP 429");

        Assert.AreEqual(now.AddMinutes(10), nextAttempt);
    }

    [TestMethod]
    public async Task Success_Resets_Failure_Count_And_Next_Failure_Starts_At_One_Minute()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var store = CreateStore(context);
        var now = context.TimeProvider.GetUtcNow();
        await store.RecordFailureAsync("recovering-source", now, null, "first");
        await store.RecordFailureAsync("recovering-source", now, null, "second");

        await store.RecordSuccessAsync("recovering-source", now.AddMinutes(3));

        var reset = await store.GetDecisionAsync("recovering-source", now.AddMinutes(3));
        Assert.IsTrue(reset.CanAttempt);
        Assert.AreEqual(0, reset.FailureCount);
        Assert.IsNull(reset.NextAttemptAt);

        var nextAttempt = await store.RecordFailureAsync(
            "recovering-source",
            now.AddMinutes(4),
            retryAfter: null,
            error: "failed again");
        Assert.AreEqual(now.AddMinutes(5), nextAttempt);
    }

    private static SourceBackoffStore CreateStore(RepositoryTestContext context) =>
        new(context.Options, new SourceBackoffOptions { JitterRatio = 0 });
}
