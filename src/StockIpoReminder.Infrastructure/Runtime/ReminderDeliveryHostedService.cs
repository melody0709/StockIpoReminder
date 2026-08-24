using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class ReminderDeliveryHostedService : BackgroundService
{
    private readonly IIpoRepository _repository;
    private readonly ReminderLifecycleService _lifecycleService;
    private readonly DailyHealthSummaryCoordinator _healthSummaryCoordinator;
    private readonly IReminderSink _sink;
    private readonly TimeProvider _timeProvider;
    private readonly ILogger<ReminderDeliveryHostedService> _logger;

    public ReminderDeliveryHostedService(
        IIpoRepository repository,
        ReminderLifecycleService lifecycleService,
        DailyHealthSummaryCoordinator healthSummaryCoordinator,
        IReminderSink sink,
        TimeProvider timeProvider,
        ILogger<ReminderDeliveryHostedService> logger)
    {
        _repository = repository;
        _lifecycleService = lifecycleService;
        _healthSummaryCoordinator = healthSummaryCoordinator;
        _sink = sink;
        _timeProvider = timeProvider;
        _logger = logger;
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        while (!stoppingToken.IsCancellationRequested)
        {
            var now = ChinaTime.Now(_timeProvider);
            try
            {
                await _repository.TouchHeartbeatAsync("delivery", now, stoppingToken).ConfigureAwait(false);
                await _lifecycleService.RefreshAsync(now, stoppingToken).ConfigureAwait(false);
                await _healthSummaryCoordinator.TrySendAsync(now, stoppingToken).ConfigureAwait(false);

                var reminders = await _repository.ClaimDueRemindersAsync(now, TimeSpan.FromMinutes(2), 20, stoppingToken).ConfigureAwait(false);
                foreach (var reminder in reminders)
                {
                    try
                    {
                        await _sink.ShowAsync(reminder, stoppingToken).ConfigureAwait(false);
                        await _repository.CompleteReminderAsync(reminder.OutboxId, ChinaTime.Now(_timeProvider), "wpf+toast", stoppingToken).ConfigureAwait(false);
                    }
                    catch (Exception ex) when (ex is not OperationCanceledException)
                    {
                        _logger.LogError(ex, "提醒 {OutboxId} 送达失败", reminder.OutboxId);
                        var delay = TimeSpan.FromMinutes(Math.Min(10, Math.Max(1, reminder.AttemptCount)));
                        await _repository.FailReminderAsync(reminder.OutboxId, now.Add(delay), ex.Message, stoppingToken).ConfigureAwait(false);
                    }
                }
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                break;
            }
            catch (Exception ex)
            {
                _logger.LogError(ex, "提醒投递循环异常");
            }

            await Task.Delay(TimeSpan.FromSeconds(10), stoppingToken).ConfigureAwait(false);
        }
    }
}
