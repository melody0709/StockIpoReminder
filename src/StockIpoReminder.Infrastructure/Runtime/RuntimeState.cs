using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed record SystemClockSample
{
    public required string Source { get; init; }
    public DateTimeOffset? ServerTime { get; init; }
    public TimeSpan? Offset { get; init; }
    public string? Error { get; init; }
}

public sealed record SystemClockSnapshot
{
    public HealthState State { get; init; } = HealthState.Unknown;
    public DateTimeOffset? CheckedAt { get; init; }
    public TimeSpan? EstimatedOffset { get; init; }
    public int ValidSampleCount { get; init; }
    public int ExpectedSampleCount { get; init; }
    public string Message { get; init; } = "系统时间尚未检查";
    public IReadOnlyList<SystemClockSample> Samples { get; init; } = [];
}

public sealed record RuntimeSnapshot
{
    public bool IsSynchronizing { get; init; }
    public DateTimeOffset? LastSyncStartedAt { get; init; }
    public DateTimeOffset? LastSyncCompletedAt { get; init; }
    public bool? LastSyncSucceeded { get; init; }
    public int LastCandidateCount { get; init; }
    public int LastEventCount { get; init; }
    public string StatusText { get; init; } = "正在启动";
    public string? LastError { get; init; }
    public SystemClockSnapshot Clock { get; init; } = new();
}

public sealed class RuntimeState
{
    private readonly object _gate = new();
    private RuntimeSnapshot _snapshot = new();

    public event EventHandler<RuntimeSnapshot>? Changed;

    public RuntimeSnapshot Snapshot
    {
        get
        {
            lock (_gate)
            {
                return _snapshot;
            }
        }
    }

    public void Update(Func<RuntimeSnapshot, RuntimeSnapshot> update)
    {
        RuntimeSnapshot snapshot;
        lock (_gate)
        {
            _snapshot = update(_snapshot);
            snapshot = _snapshot;
        }

        Changed?.Invoke(this, snapshot);
    }
}
