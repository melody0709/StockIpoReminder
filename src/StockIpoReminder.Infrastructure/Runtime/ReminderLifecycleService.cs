using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class ReminderLifecycleService
{
    private readonly IIpoRepository _repository;

    public ReminderLifecycleService(IIpoRepository repository)
    {
        _repository = repository;
    }

    public async Task RefreshAsync(DateTimeOffset now, CancellationToken cancellationToken = default)
    {
        var today = DateOnly.FromDateTime(now.DateTime);
        var settings = await _repository.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
        var events = await _repository.GetEventsAsync(today, today, cancellationToken).ConfigureAwait(false);
        foreach (var ipoEvent in events)
        {
            var lifecycleStatus = ipoEvent.LifecycleStatus;
            if (lifecycleStatus == IpoLifecycleStatus.Scheduled)
            {
                await _repository.SetLifecycleStatusAsync(
                    ipoEvent.Id,
                    ipoEvent.EventVersion,
                    IpoLifecycleStatus.ActiveUnconfirmed,
                    now,
                    cancellationToken).ConfigureAwait(false);
                lifecycleStatus = IpoLifecycleStatus.ActiveUnconfirmed;
            }

            if (lifecycleStatus is IpoLifecycleStatus.ActiveUnconfirmed or IpoLifecycleStatus.AcknowledgedNeedsReview)
            {
                var cutoff = ReminderPlanner.GetEffectiveSafetyCutoff(ipoEvent, settings);
                if (now >= ChinaTime.At(today, cutoff))
                {
                    await _repository.SetLifecycleStatusAsync(
                        ipoEvent.Id,
                        ipoEvent.EventVersion,
                        IpoLifecycleStatus.ExpiredUnconfirmed,
                        now,
                        cancellationToken).ConfigureAwait(false);
                }
            }
        }

        await _repository.TouchHeartbeatAsync("scheduler", now, cancellationToken).ConfigureAwait(false);
    }
}
