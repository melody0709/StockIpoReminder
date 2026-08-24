using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class TrayStatusHostedService : BackgroundService
{
    private readonly ReminderManagementService _managementService;
    private readonly DesktopReminderSink _sink;
    private readonly RuntimeState _runtimeState;
    private readonly TimeProvider _timeProvider;
    private readonly ILogger<TrayStatusHostedService> _logger;

    public TrayStatusHostedService(
        ReminderManagementService managementService,
        DesktopReminderSink sink,
        RuntimeState runtimeState,
        TimeProvider timeProvider,
        ILogger<TrayStatusHostedService> logger)
    {
        _managementService = managementService;
        _sink = sink;
        _runtimeState = runtimeState;
        _timeProvider = timeProvider;
        _logger = logger;
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        while (!stoppingToken.IsCancellationRequested)
        {
            try
            {
                var today = ChinaTime.Today(_timeProvider);
                var settings = await _managementService.GetSettingsAsync(stoppingToken).ConfigureAwait(false);
                var events = await _managementService.GetEventsAsync(today, today, stoppingToken).ConfigureAwait(false);
                var pending = events.Count(ipoEvent => settings.IsExchangeEnabled(ipoEvent.Exchange)
                    && ipoEvent.LifecycleStatus is IpoLifecycleStatus.Scheduled
                        or IpoLifecycleStatus.ActiveUnconfirmed
                        or IpoLifecycleStatus.AcknowledgedNeedsReview);
                var health = await _managementService.GetHealthSummaryAsync(stoppingToken).ConfigureAwait(false);
                var clockState = _runtimeState.Snapshot.Clock.State;
                _sink.UpdateTrayStatus(
                    pending,
                    health.OverallState != HealthState.Healthy
                    || clockState is HealthState.Warning or HealthState.Failed);
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                break;
            }
            catch (Exception ex)
            {
                _logger.LogWarning(ex, "无法刷新托盘状态");
            }

            await Task.Delay(TimeSpan.FromSeconds(30), stoppingToken).ConfigureAwait(false);
        }
    }
}
