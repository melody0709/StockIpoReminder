using StockIpoReminder.Core.Abstractions;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class DailyHealthSummaryCoordinator
{
    private static readonly TimeOnly DeliveryStart = new(8, 0);
    private readonly IIpoRepository _repository;
    private readonly IReminderSink _sink;

    public DailyHealthSummaryCoordinator(IIpoRepository repository, IReminderSink sink)
    {
        _repository = repository;
        _sink = sink;
    }

    public async Task<bool> TrySendAsync(DateTimeOffset now, CancellationToken cancellationToken)
    {
        if (TimeOnly.FromDateTime(now.DateTime) < DeliveryStart)
        {
            return false;
        }

        var settings = await _repository.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
        var today = DateOnly.FromDateTime(now.DateTime);
        if (!settings.DailyHealthSummaryEnabled
            || !await _repository.TryMarkHealthSummarySentAsync(today, now, cancellationToken).ConfigureAwait(false))
        {
            return false;
        }

        var summary = await _repository.GetHealthSummaryAsync(today, now, cancellationToken).ConfigureAwait(false);
        await _sink.ShowHealthSummaryAsync(summary, cancellationToken).ConfigureAwait(false);
        return true;
    }
}
