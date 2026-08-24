using StockIpoReminder.Core.Abstractions;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class RecoveryEventOptions
{
    public TimeSpan DebounceInterval { get; init; } = TimeSpan.FromSeconds(5);
}

public enum RecoveryEventKind
{
    Resume = 1,
    SessionUnlock = 2,
    NetworkAvailable = 3,
}

public sealed record RecoveryDispatchResult(
    RecoveryEventKind Kind,
    string Reason,
    bool Dispatched,
    bool Debounced,
    long Sequence);

public sealed class RecoveryEventCoordinator
{
    private readonly object _gate = new();
    private readonly ISyncTrigger _syncTrigger;
    private readonly ISystemClockCheckTrigger _clockCheckTrigger;
    private readonly TimeProvider _timeProvider;
    private readonly TimeSpan _debounceInterval;
    private long? _lastAcceptedTimestamp;
    private long _sequence;

    public RecoveryEventCoordinator(
        ISyncTrigger syncTrigger,
        ISystemClockCheckTrigger clockCheckTrigger,
        TimeProvider timeProvider,
        RecoveryEventOptions options)
    {
        ArgumentNullException.ThrowIfNull(syncTrigger);
        ArgumentNullException.ThrowIfNull(clockCheckTrigger);
        ArgumentNullException.ThrowIfNull(timeProvider);
        ArgumentNullException.ThrowIfNull(options);
        if (options.DebounceInterval <= TimeSpan.Zero)
        {
            throw new ArgumentOutOfRangeException(
                nameof(options),
                options.DebounceInterval,
                "恢复事件防抖时间必须大于零。");
        }

        _syncTrigger = syncTrigger;
        _clockCheckTrigger = clockCheckTrigger;
        _timeProvider = timeProvider;
        _debounceInterval = options.DebounceInterval;
    }

    public TimeSpan DebounceInterval => _debounceInterval;

    public RecoveryDispatchResult Dispatch(RecoveryEventKind kind)
    {
        var reason = GetReason(kind);
        var timestamp = _timeProvider.GetTimestamp();
        long sequence;
        lock (_gate)
        {
            if (_lastAcceptedTimestamp is { } previous
                && _timeProvider.GetElapsedTime(previous, timestamp) < _debounceInterval)
            {
                return new RecoveryDispatchResult(kind, reason, Dispatched: false, Debounced: true, _sequence);
            }

            _lastAcceptedTimestamp = timestamp;
            sequence = ++_sequence;
        }

        _syncTrigger.RequestSync(reason);
        _clockCheckTrigger.RequestCheck(reason);
        return new RecoveryDispatchResult(kind, reason, Dispatched: true, Debounced: false, sequence);
    }

    private static string GetReason(RecoveryEventKind kind) => kind switch
    {
        RecoveryEventKind.Resume => "电脑从休眠恢复",
        RecoveryEventKind.SessionUnlock => "Windows 会话解锁",
        RecoveryEventKind.NetworkAvailable => "网络连接恢复",
        _ => throw new ArgumentOutOfRangeException(nameof(kind), kind, "未知的恢复事件类型。"),
    };
}
