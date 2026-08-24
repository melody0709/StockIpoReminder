using System.Threading.Channels;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class SyncCoordinatorHostedService : BackgroundService, ISyncTrigger
{
    private readonly Channel<string> _requests = Channel.CreateUnbounded<string>(new UnboundedChannelOptions
    {
        SingleReader = true,
        SingleWriter = false,
    });
    private readonly SynchronizationService _synchronizationService;
    private readonly IIpoRepository _repository;
    private readonly TimeProvider _timeProvider;
    private readonly ILogger<SyncCoordinatorHostedService> _logger;

    public SyncCoordinatorHostedService(
        SynchronizationService synchronizationService,
        IIpoRepository repository,
        TimeProvider timeProvider,
        ILogger<SyncCoordinatorHostedService> logger)
    {
        _synchronizationService = synchronizationService;
        _repository = repository;
        _timeProvider = timeProvider;
        _logger = logger;
    }

    public void RequestSync(string reason) => _requests.Writer.TryWrite(reason);

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        await _repository.InitializeAsync(stoppingToken).ConfigureAwait(false);
        RequestSync("程序启动");

        while (!stoppingToken.IsCancellationRequested)
        {
            try
            {
                var settings = await _repository.GetSettingsAsync(stoppingToken).ConfigureAwait(false);
                var today = ChinaTime.Today(_timeProvider);
                var active = (await _repository.GetPendingEventsAsync(today, stoppingToken).ConfigureAwait(false))
                    .Any(ipoEvent => settings.IsExchangeEnabled(ipoEvent.Exchange));
                var minutes = active ? settings.ActiveDaySyncMinutes : settings.NormalSyncMinutes;
                var baseDelay = TimeSpan.FromMinutes(Math.Clamp(minutes, 5, 120));
                var delay = Task.Delay(AddJitter(baseDelay, active, Random.Shared.NextDouble()), stoppingToken);
                var read = _requests.Reader.ReadAsync(stoppingToken).AsTask();
                var winner = await Task.WhenAny(delay, read).ConfigureAwait(false);
                var reason = winner == read ? await read.ConfigureAwait(false) : "定时同步";
                while (_requests.Reader.TryRead(out var queued))
                {
                    reason = queued;
                }

                await _synchronizationService.SynchronizeAsync(reason, stoppingToken).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                break;
            }
            catch (Exception ex)
            {
                _logger.LogError(ex, "同步协调器异常，将在一分钟后重试");
                await Task.Delay(TimeSpan.FromMinutes(1), stoppingToken).ConfigureAwait(false);
            }
        }
    }

    public static TimeSpan AddJitter(TimeSpan baseDelay, bool activeDay, double randomSample)
    {
        var maximumSeconds = activeDay ? 20 : 90;
        var sample = Math.Clamp(randomSample, 0, 1);
        return baseDelay + TimeSpan.FromSeconds(maximumSeconds * sample);
    }
}
