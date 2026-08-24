using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class RecoveryEventCoordinatorTests
{
    private static readonly string[] ResumeAndUnlockReasons = ["电脑从休眠恢复", "Windows 会话解锁"];

    [TestMethod]
    public void DispatchTriggersBothChannelsAndDebouncesBurst()
    {
        var sync = new RecordingSyncTrigger();
        var clock = new RecordingClockTrigger();
        var time = new ManualTimeProvider();
        var coordinator = new RecoveryEventCoordinator(
            sync,
            clock,
            time,
            new RecoveryEventOptions { DebounceInterval = TimeSpan.FromSeconds(5) });

        var resume = coordinator.Dispatch(RecoveryEventKind.Resume);
        var burstUnlock = coordinator.Dispatch(RecoveryEventKind.SessionUnlock);

        Assert.IsTrue(resume.Dispatched);
        Assert.IsFalse(resume.Debounced);
        Assert.AreEqual(1L, resume.Sequence);
        Assert.IsFalse(burstUnlock.Dispatched);
        Assert.IsTrue(burstUnlock.Debounced);
        Assert.AreEqual(1, sync.Reasons.Count);
        Assert.AreEqual(1, clock.Reasons.Count);
        Assert.AreEqual(sync.Reasons[0], clock.Reasons[0]);

        time.Advance(TimeSpan.FromSeconds(5));
        var unlock = coordinator.Dispatch(RecoveryEventKind.SessionUnlock);

        Assert.IsTrue(unlock.Dispatched);
        Assert.AreEqual(2L, unlock.Sequence);
        CollectionAssert.AreEqual(ResumeAndUnlockReasons, sync.Reasons);
        CollectionAssert.AreEqual(sync.Reasons, clock.Reasons);
    }

    [TestMethod]
    [DataRow(RecoveryEventKind.Resume, "电脑从休眠恢复")]
    [DataRow(RecoveryEventKind.SessionUnlock, "Windows 会话解锁")]
    [DataRow(RecoveryEventKind.NetworkAvailable, "网络连接恢复")]
    public void DispatchUsesStableReasonForEveryRecoveryKind(RecoveryEventKind kind, string expectedReason)
    {
        var sync = new RecordingSyncTrigger();
        var clock = new RecordingClockTrigger();
        var coordinator = new RecoveryEventCoordinator(
            sync,
            clock,
            new ManualTimeProvider(),
            new RecoveryEventOptions());

        var result = coordinator.Dispatch(kind);

        Assert.IsTrue(result.Dispatched);
        Assert.AreEqual(expectedReason, result.Reason);
        Assert.AreEqual(expectedReason, sync.Reasons.Single());
        Assert.AreEqual(expectedReason, clock.Reasons.Single());
    }

    private sealed class RecordingSyncTrigger : ISyncTrigger
    {
        public List<string> Reasons { get; } = [];

        public void RequestSync(string reason) => Reasons.Add(reason);
    }

    private sealed class RecordingClockTrigger : ISystemClockCheckTrigger
    {
        public List<string> Reasons { get; } = [];

        public void RequestCheck(string reason) => Reasons.Add(reason);
    }

    private sealed class ManualTimeProvider : TimeProvider
    {
        private DateTimeOffset _utcNow = new(2026, 8, 24, 0, 0, 0, TimeSpan.Zero);
        private long _timestamp;

        public override long TimestampFrequency => TimeSpan.TicksPerSecond;

        public override DateTimeOffset GetUtcNow() => _utcNow;

        public override long GetTimestamp() => _timestamp;

        public void Advance(TimeSpan elapsed)
        {
            _utcNow = _utcNow.Add(elapsed);
            _timestamp = checked(_timestamp + elapsed.Ticks);
        }
    }
}
