using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class DailyHealthSummaryCoordinatorTests
{
    [TestMethod]
    public async Task SendsAtEightOncePerShanghaiDate()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        await context.Repository.SaveSettingsAsync(new AppSettings { DailyHealthSummaryEnabled = true });
        var sink = new RecordingReminderSink();
        var coordinator = new DailyHealthSummaryCoordinator(context.Repository, sink);
        var date = new DateOnly(2026, 8, 24);

        Assert.IsFalse(await coordinator.TrySendAsync(ChinaTime.At(date, new TimeOnly(7, 59)), CancellationToken.None));
        Assert.AreEqual(0, sink.HealthSummaries.Count);

        Assert.IsTrue(await coordinator.TrySendAsync(ChinaTime.At(date, new TimeOnly(8, 0)), CancellationToken.None));
        Assert.AreEqual(1, sink.HealthSummaries.Count);

        Assert.IsFalse(await coordinator.TrySendAsync(ChinaTime.At(date, new TimeOnly(8, 30)), CancellationToken.None));
        Assert.AreEqual(1, sink.HealthSummaries.Count);

        Assert.IsTrue(await coordinator.TrySendAsync(ChinaTime.At(date.AddDays(1), new TimeOnly(8, 0)), CancellationToken.None));
        Assert.AreEqual(2, sink.HealthSummaries.Count);
    }

    private sealed class RecordingReminderSink : IReminderSink
    {
        public List<HealthSummary> HealthSummaries { get; } = [];

        public Task ShowAsync(ReminderDelivery reminder, CancellationToken cancellationToken) => Task.CompletedTask;

        public Task ShowHealthSummaryAsync(HealthSummary summary, CancellationToken cancellationToken)
        {
            HealthSummaries.Add(summary);
            return Task.CompletedTask;
        }
    }
}
