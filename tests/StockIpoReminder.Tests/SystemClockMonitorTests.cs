using System.Net;
using Microsoft.Extensions.Logging.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class SystemClockMonitorTests
{
    [TestMethod]
    public async Task Two_Independent_Samples_Within_Threshold_Are_Healthy()
    {
        var now = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero);
        var timeProvider = new RepositoryTestContext.MutableTimeProvider(now);
        using var client = CreateClient(request => request.RequestUri!.Host == "clock-one.test"
            ? Response(now.AddSeconds(20))
            : Response(now.AddSeconds(22)));
        var runtime = new RuntimeState();
        var monitor = CreateMonitor(client, timeProvider, runtime);

        var result = await monitor.CheckAsync("test");

        Assert.AreEqual(HealthState.Healthy, result.State);
        Assert.AreEqual(2, result.ValidSampleCount);
        Assert.AreEqual(TimeSpan.FromSeconds(21), result.EstimatedOffset);
        Assert.AreEqual(result, runtime.Snapshot.Clock);
    }

    [TestMethod]
    public async Task One_Valid_Sample_Is_Warning_Not_A_False_Time_Error()
    {
        var now = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero);
        var timeProvider = new RepositoryTestContext.MutableTimeProvider(now);
        using var client = CreateClient(request => request.RequestUri!.Host == "clock-one.test"
            ? Response(now)
            : new HttpResponseMessage(HttpStatusCode.OK));
        var runtime = new RuntimeState();
        var monitor = CreateMonitor(client, timeProvider, runtime);

        var result = await monitor.CheckAsync("test");

        Assert.AreEqual(HealthState.Warning, result.State);
        Assert.AreEqual(1, result.ValidSampleCount);
        StringAssert.Contains(result.Message, "样本不足");
    }

    [TestMethod]
    public async Task Offset_Over_Five_Minutes_Is_Failed_But_Does_Not_Change_Task_State()
    {
        var date = new DateOnly(2026, 8, 24);
        var now = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero);
        var timeProvider = new RepositoryTestContext.MutableTimeProvider(now);
        using var client = CreateClient(_ => Response(now.AddMinutes(6)));
        var runtime = new RuntimeState();
        var monitor = CreateMonitor(client, timeProvider, runtime);
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var seeded = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:601301",
            date,
            lifecycle: IpoLifecycleStatus.ActiveUnconfirmed,
            status: IssueStatus.Active));

        var result = await monitor.CheckAsync("test");

        Assert.AreEqual(HealthState.Failed, result.State);
        Assert.AreEqual(
            IpoLifecycleStatus.ActiveUnconfirmed,
            (await context.Repository.GetEventAsync(seeded.Event.Id))!.LifecycleStatus);
    }

    private static SystemClockMonitor CreateMonitor(
        HttpClient client,
        TimeProvider timeProvider,
        RuntimeState runtime) => new(
        client,
        new SystemClockOptions
        {
            Endpoints =
            [
                new Uri("https://clock-one.test/"),
                new Uri("https://clock-two.test/"),
            ],
        },
        runtime,
        timeProvider,
        NullLogger<SystemClockMonitor>.Instance);

    private static HttpClient CreateClient(Func<HttpRequestMessage, HttpResponseMessage> response) =>
        new(new DelegateHandler(response)) { Timeout = TimeSpan.FromSeconds(5) };

    private static HttpResponseMessage Response(DateTimeOffset date)
    {
        var response = new HttpResponseMessage(HttpStatusCode.OK);
        response.Headers.Date = date;
        return response;
    }

    private sealed class DelegateHandler : HttpMessageHandler
    {
        private readonly Func<HttpRequestMessage, HttpResponseMessage> _response;

        public DelegateHandler(Func<HttpRequestMessage, HttpResponseMessage> response) => _response = response;

        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request,
            CancellationToken cancellationToken) =>
            Task.FromResult(_response(request));
    }
}
