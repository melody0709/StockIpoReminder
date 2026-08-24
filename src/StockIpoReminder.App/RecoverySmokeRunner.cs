using System.IO;
using System.Reflection;
using System.Text.Json;
using Microsoft.Extensions.Logging;
using Microsoft.Win32;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class RecoverySmokeRunner
{
    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = true };
    private readonly ApplicationRuntimeOptions _runtimeOptions;
    private readonly ILoggerFactory _loggerFactory;

    public RecoverySmokeRunner(
        ApplicationRuntimeOptions runtimeOptions,
        ILoggerFactory loggerFactory)
    {
        _runtimeOptions = runtimeOptions;
        _loggerFactory = loggerFactory;
    }

    public async Task<bool> RunAsync(CancellationToken cancellationToken = default)
    {
        if (_runtimeOptions.RecoverySmokeReport is null)
        {
            throw new InvalidOperationException("恢复事件 smoke 报告路径未配置。");
        }

        var reportDirectory = Path.GetDirectoryName(_runtimeOptions.RecoverySmokeReport)
            ?? throw new InvalidOperationException("恢复事件 smoke 报告目录无效。");
        Directory.CreateDirectory(reportDirectory);
        var checks = new Dictionary<string, bool>(StringComparer.Ordinal);
        var sync = new RecordingSyncTrigger();
        var clock = new RecordingClockCheckTrigger();
        var time = new ManualTimeProvider();
        var options = new RecoveryEventOptions();
        var coordinator = new RecoveryEventCoordinator(sync, clock, time, options);
        var service = new RecoveryEventService(
            coordinator,
            _loggerFactory.CreateLogger<RecoveryEventService>());
        var dispatches = new List<RecoveryDispatchResult>();
        string? error = null;

        try
        {
            Check(checks, "suspendIsIgnored", service.HandlePowerMode(PowerModes.Suspend) is null);
            Check(checks, "sessionLockIsIgnored", service.HandleSessionSwitch(SessionSwitchReason.SessionLock) is null);
            Check(checks, "networkLossIsIgnored", service.HandleNetworkAvailability(isAvailable: false) is null);

            var resume = Require(service.HandlePowerMode(PowerModes.Resume));
            dispatches.Add(resume);
            Check(checks, "resumeDispatches", resume.Dispatched && !resume.Debounced);

            var burstUnlock = Require(service.HandleSessionSwitch(SessionSwitchReason.SessionUnlock));
            dispatches.Add(burstUnlock);
            Check(checks, "burstRecoveryIsDebounced", !burstUnlock.Dispatched && burstUnlock.Debounced);
            Check(checks, "debouncedEventDoesNotTriggerChannels", sync.Reasons.Count == 1 && clock.Reasons.Count == 1);

            time.Advance(options.DebounceInterval);
            var unlock = Require(service.HandleSessionSwitch(SessionSwitchReason.SessionUnlock));
            dispatches.Add(unlock);
            Check(checks, "unlockDispatchesAfterDebounce", unlock.Dispatched && unlock.Sequence == 2);

            time.Advance(options.DebounceInterval);
            var network = Require(service.HandleNetworkAvailability(isAvailable: true));
            dispatches.Add(network);
            Check(checks, "networkRecoveryDispatchesAfterDebounce", network.Dispatched && network.Sequence == 3);

            var burstResume = Require(service.HandlePowerMode(PowerModes.Resume));
            dispatches.Add(burstResume);
            Check(checks, "secondBurstIsDebounced", !burstResume.Dispatched && burstResume.Debounced);
            Check(checks, "productionDebounceIsFiveSeconds", options.DebounceInterval == TimeSpan.FromSeconds(5));
            Check(checks, "acceptedEventsTriggerSyncAndClockCheck",
                sync.Reasons.Count == 3
                && clock.Reasons.Count == 3
                && sync.Reasons.SequenceEqual(clock.Reasons, StringComparer.Ordinal));
            Check(checks, "allRecoveryKindsAreCovered", sync.Reasons.SequenceEqual(
                ["电脑从休眠恢复", "Windows 会话解锁", "网络连接恢复"],
                StringComparer.Ordinal));
        }
        catch (Exception ex)
        {
            error = DescribeException(ex, reportDirectory);
            Check(checks, "runnerCompletedWithoutException", false);
        }

        if (!checks.ContainsKey("runnerCompletedWithoutException"))
        {
            Check(checks, "runnerCompletedWithoutException", true);
        }

        var failedChecks = checks.Where(static pair => !pair.Value).Select(static pair => pair.Key).ToArray();
        var report = new
        {
            success = failedChecks.Length == 0,
            version = Assembly.GetEntryAssembly()?.GetName().Version?.ToString(3) ?? "unknown",
            generatedAtUtc = DateTimeOffset.UtcNow,
            mode = "deterministic-recovery-event-simulation",
            debounceSeconds = options.DebounceInterval.TotalSeconds,
            checks,
            failedChecks,
            dispatches = dispatches.Select(static result => new
            {
                kind = result.Kind.ToString(),
                result.Reason,
                result.Dispatched,
                result.Debounced,
                result.Sequence,
            }),
            syncReasons = sync.Reasons,
            clockCheckReasons = clock.Reasons,
            error,
        };
        await WriteReportAsync(_runtimeOptions.RecoverySmokeReport, report, cancellationToken).ConfigureAwait(false);
        return failedChecks.Length == 0;
    }

    private static RecoveryDispatchResult Require(RecoveryDispatchResult? result) =>
        result ?? throw new InvalidOperationException("恢复事件未进入协调器。");

    private static void Check(Dictionary<string, bool> checks, string name, bool value) => checks[name] = value;

    private string DescribeException(Exception exception, string reportDirectory)
    {
        var value = DiagnosticRedactor.Redact($"{exception.GetType().Name}: {exception.Message}");
        foreach (var (path, replacement) in new[]
        {
            (_runtimeOptions.DataRoot, "<data-root>"),
            (reportDirectory, "<report-directory>"),
            (Environment.CurrentDirectory, "<working-directory>"),
            (AppContext.BaseDirectory, "<application-directory>"),
            (Path.GetTempPath(), "<temp-directory>"),
        })
        {
            if (!string.IsNullOrWhiteSpace(path))
            {
                value = value.Replace(path, replacement, StringComparison.OrdinalIgnoreCase);
            }
        }

        return value;
    }

    private static async Task WriteReportAsync(string path, object report, CancellationToken cancellationToken)
    {
        var temporaryPath = path + $".{Guid.NewGuid():N}.tmp";
        await File.WriteAllTextAsync(temporaryPath, JsonSerializer.Serialize(report, JsonOptions), cancellationToken).ConfigureAwait(false);
        File.Move(temporaryPath, path, overwrite: true);
    }

    private sealed class RecordingSyncTrigger : ISyncTrigger
    {
        public List<string> Reasons { get; } = [];

        public void RequestSync(string reason) => Reasons.Add(reason);
    }

    private sealed class RecordingClockCheckTrigger : ISystemClockCheckTrigger
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
