using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class SyncCoordinatorHostedServiceTests
{
    [TestMethod]
    public void AddJitter_Uses_At_Most_Ninety_Seconds_On_Normal_Days()
    {
        var baseline = TimeSpan.FromMinutes(30);

        Assert.AreEqual(baseline, SyncCoordinatorHostedService.AddJitter(baseline, activeDay: false, randomSample: 0));
        Assert.AreEqual(baseline.Add(TimeSpan.FromSeconds(45)), SyncCoordinatorHostedService.AddJitter(baseline, activeDay: false, randomSample: 0.5));
        Assert.AreEqual(baseline.Add(TimeSpan.FromSeconds(90)), SyncCoordinatorHostedService.AddJitter(baseline, activeDay: false, randomSample: 1));
        Assert.AreEqual(baseline.Add(TimeSpan.FromSeconds(90)), SyncCoordinatorHostedService.AddJitter(baseline, activeDay: false, randomSample: 2));
    }

    [TestMethod]
    public void AddJitter_Uses_At_Most_Twenty_Seconds_On_Active_Days()
    {
        var baseline = TimeSpan.FromMinutes(10);

        Assert.AreEqual(baseline, SyncCoordinatorHostedService.AddJitter(baseline, activeDay: true, randomSample: -1));
        Assert.AreEqual(baseline.Add(TimeSpan.FromSeconds(10)), SyncCoordinatorHostedService.AddJitter(baseline, activeDay: true, randomSample: 0.5));
        Assert.AreEqual(baseline.Add(TimeSpan.FromSeconds(20)), SyncCoordinatorHostedService.AddJitter(baseline, activeDay: true, randomSample: 1));
    }
}
