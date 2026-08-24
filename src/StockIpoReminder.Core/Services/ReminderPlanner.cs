using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Core.Services;

public sealed class ReminderPlanner
{
    public static TimeOnly GetEffectiveSafetyCutoff(IpoEvent ipoEvent, AppSettings settings)
    {
        var sessions = ResolveSessions(ipoEvent, settings);
        var officialEnd = sessions.Length > 0 ? sessions[^1].OfficialEnd : new TimeOnly(15, 0);
        return settings.SafetyCutoff > officialEnd ? officialEnd : settings.SafetyCutoff;
    }

    public IReadOnlyList<ReminderScheduleItem> Plan(IpoEvent ipoEvent, AppSettings settings)
    {
        if (ipoEvent.ApplyDate is null
            || ipoEvent.IsTerminal
            || ipoEvent.LifecycleStatus == IpoLifecycleStatus.Acknowledged
            || !settings.IsExchangeEnabled(ipoEvent.Exchange))
        {
            return [];
        }

        var sessions = ResolveSessions(ipoEvent, settings);
        if (sessions.Length == 0)
        {
            return [];
        }

        var date = ipoEvent.ApplyDate.Value;
        var first = sessions[0];
        var cutoff = GetEffectiveSafetyCutoff(ipoEvent, settings);

        var due = new Dictionary<DateTimeOffset, ReminderLevel>();
        Add(due, ChinaTime.At(date.AddDays(-1), new TimeOnly(20, 0)), ReminderLevel.Advance);
        Add(due, ChinaTime.At(date, new TimeOnly(8, 30)), ReminderLevel.Morning);

        var brokerStart = ipoEvent.Exchange switch
        {
            Exchange.Shanghai => settings.ShanghaiBrokerAcceptStart,
            Exchange.Shenzhen => settings.ShenzhenBrokerAcceptStart,
            Exchange.Beijing => settings.BeijingBrokerAcceptStart,
            _ => first.BrokerAcceptStart,
        };
        var brokerReminderEnabled = ipoEvent.Exchange != Exchange.Beijing || settings.BeijingReservationSupported;
        if (brokerReminderEnabled && brokerStart is not null && brokerStart.Value < first.OfficialStart)
        {
            Add(due, ChinaTime.At(date, brokerStart.Value), ReminderLevel.BrokerOpening);
        }

        Add(due, ChinaTime.At(date, first.OfficialStart.AddMinutes(-5)), ReminderLevel.MarketOpening);

        foreach (var session in sessions)
        {
            for (var cursor = session.OfficialStart; cursor < session.OfficialEnd; cursor = cursor.AddHours(1))
            {
                if (cursor < cutoff)
                {
                    Add(due, ChinaTime.At(date, cursor), ReminderLevel.Hourly);
                }
            }
        }

        AddIfBeforeCutoff(due, date, new TimeOnly(11, 20), cutoff, ReminderLevel.NoonBoundary);
        AddIfBeforeCutoff(due, date, new TimeOnly(12, 55), cutoff, ReminderLevel.AfternoonOpening);

        AddRange(due, date, cutoff.AddMinutes(-60), cutoff.AddMinutes(-30), 15, ReminderLevel.FifteenMinutes);
        AddRange(due, date, cutoff.AddMinutes(-30), cutoff.AddMinutes(-10), 5, ReminderLevel.FiveMinutes);
        AddRange(due, date, cutoff.AddMinutes(-10), cutoff, 2, ReminderLevel.TwoMinutes);
        Add(due, ChinaTime.At(date, cutoff), ReminderLevel.Final);

        return due
            .OrderBy(static x => x.Key)
            .Select(x => new ReminderScheduleItem
            {
                IpoEventId = ipoEvent.Id,
                EventVersion = ipoEvent.EventVersion,
                DueAt = x.Key,
                Level = x.Value,
                DedupeKey = $"{ipoEvent.Id}:{ipoEvent.EventVersion}:{x.Key.UtcTicks}:{(int)x.Value}",
            })
            .ToArray();
    }

    private static SubscriptionSession[] ResolveSessions(IpoEvent ipoEvent, AppSettings settings) =>
        ipoEvent.Sessions.Count > 0
            ? ipoEvent.Sessions.OrderBy(static x => x.SessionNumber).ToArray()
            : MarketSessionFactory.CreateDefault(ipoEvent.Exchange, settings).ToArray();

    private static void AddRange(
        IDictionary<DateTimeOffset, ReminderLevel> due,
        DateOnly date,
        TimeOnly start,
        TimeOnly endExclusive,
        int minutes,
        ReminderLevel level)
    {
        for (var cursor = start; cursor < endExclusive; cursor = cursor.AddMinutes(minutes))
        {
            Add(due, ChinaTime.At(date, cursor), level);
        }
    }

    private static void AddIfBeforeCutoff(
        IDictionary<DateTimeOffset, ReminderLevel> due,
        DateOnly date,
        TimeOnly time,
        TimeOnly cutoff,
        ReminderLevel level)
    {
        if (time < cutoff)
        {
            Add(due, ChinaTime.At(date, time), level);
        }
    }

    private static void Add(IDictionary<DateTimeOffset, ReminderLevel> due, DateTimeOffset when, ReminderLevel level)
    {
        if (!due.TryGetValue(when, out var current) || level > current)
        {
            due[when] = level;
        }
    }
}
