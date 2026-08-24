using System.IO;
using System.Reflection;
using System.Text.Json;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Persistence;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class ProcessSmokeRunner
{
    public const string CrashRecoveryDedupeKey = "process-smoke:shenzhen:301001:crash-recovery";
    private static readonly TimeSpan LeaseDuration = TimeSpan.FromSeconds(2);
    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = true };

    private readonly ApplicationRuntimeOptions _runtimeOptions;
    private readonly IIpoRepository _repository;
    private readonly ReminderManagementService _managementService;
    private readonly PersistenceInspectionService _inspectionService;
    private readonly TimeProvider _timeProvider;

    public ProcessSmokeRunner(
        ApplicationRuntimeOptions runtimeOptions,
        IIpoRepository repository,
        ReminderManagementService managementService,
        PersistenceInspectionService inspectionService,
        TimeProvider timeProvider)
    {
        _runtimeOptions = runtimeOptions;
        _repository = repository;
        _managementService = managementService;
        _inspectionService = inspectionService;
        _timeProvider = timeProvider;
    }

    public async Task<bool> RunAsync(CancellationToken cancellationToken = default)
    {
        if (_runtimeOptions.ProcessSmokePhase is null || _runtimeOptions.ProcessSmokeReport is null)
        {
            throw new InvalidOperationException("进程恢复 smoke 参数未完整配置。");
        }

        var reportDirectory = Path.GetDirectoryName(_runtimeOptions.ProcessSmokeReport)
            ?? throw new InvalidOperationException("进程恢复 smoke 报告目录无效。");
        Directory.CreateDirectory(reportDirectory);
        var checks = new Dictionary<string, bool>(StringComparer.Ordinal);
        ReminderPersistenceSnapshot? persistence = null;
        IReadOnlyList<IpoEvent> events = [];
        string? error = null;

        try
        {
            if (_runtimeOptions.ProcessSmokePhase == ProcessSmokeStage.Prepare)
            {
                persistence = await PrepareAsync(checks, cancellationToken).ConfigureAwait(false);
            }
            else
            {
                persistence = await VerifyAsync(checks, cancellationToken).ConfigureAwait(false);
            }

            var today = ChinaTime.Today(_timeProvider);
            events = await _repository.GetEventsAsync(today, today, cancellationToken).ConfigureAwait(false);
            Check(checks, "stateReadableAfterStage", events.Count >= 3);
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
            stage = _runtimeOptions.ProcessSmokePhase.ToString()!.ToLowerInvariant(),
            generatedAtUtc = DateTimeOffset.UtcNow,
            processId = Environment.ProcessId,
            dataRoot = "<isolated-smoke-data-root>",
            dataRootInstance = _runtimeOptions.InstanceKey[..12],
            leaseDurationSeconds = LeaseDuration.TotalSeconds,
            checks,
            failedChecks,
            persistence = persistence is null ? null : new
            {
                outbox = persistence.Outbox.Select(static row => new
                {
                    row.OutboxId,
                    state = row.State.ToString(),
                    row.AttemptCount,
                    row.LeaseUntil,
                }),
                persistence.ReminderLogCount,
                persistence.ActiveAcknowledgementCount,
                persistence.IntegrityResult,
            },
            events = events.Select(static ipoEvent => new
            {
                ipoEvent.Id,
                lifecycle = ipoEvent.LifecycleStatus.ToString(),
                quality = ipoEvent.DataQualityStatus.ToString(),
                ipoEvent.EventVersion,
            }),
            error,
        };
        await WriteReportAsync(_runtimeOptions.ProcessSmokeReport, report, cancellationToken).ConfigureAwait(false);
        return failedChecks.Length == 0;
    }

    private async Task<ReminderPersistenceSnapshot> PrepareAsync(
        Dictionary<string, bool> checks,
        CancellationToken cancellationToken)
    {
        var shanghai = await RequireEventAsync(UiSmokeScenarioSeeder.ShanghaiEventId, cancellationToken).ConfigureAwait(false);
        await _managementService.AcknowledgeAsync(shanghai.Id, shanghai.EventVersion, cancellationToken).ConfigureAwait(false);
        var acknowledged = await RequireEventAsync(UiSmokeScenarioSeeder.ShanghaiEventId, cancellationToken).ConfigureAwait(false);
        var shenzhen = await RequireEventAsync(UiSmokeScenarioSeeder.ShenzhenEventId, cancellationToken).ConfigureAwait(false);
        var beijing = await RequireEventAsync(UiSmokeScenarioSeeder.BeijingEventId, cancellationToken).ConfigureAwait(false);

        Check(checks, "oneTaskAcknowledgedBeforeCrash", acknowledged.LifecycleStatus == IpoLifecycleStatus.Acknowledged);
        Check(checks, "twoTasksRemainPendingBeforeCrash", IsPending(shenzhen) && IsPending(beijing));

        var now = ChinaTime.Now(_timeProvider);
        var reminder = new ReminderScheduleItem
        {
            IpoEventId = shenzhen.Id,
            EventVersion = shenzhen.EventVersion,
            DueAt = now.AddSeconds(-1),
            Level = ReminderLevel.DataChanged,
            DedupeKey = CrashRecoveryDedupeKey,
        };
        await _repository.EnqueueRemindersAsync([reminder], cancellationToken).ConfigureAwait(false);
        await _repository.EnqueueRemindersAsync([reminder], cancellationToken).ConfigureAwait(false);

        var claimed = await _repository.ClaimDueRemindersAsync(now, LeaseDuration, 100, cancellationToken).ConfigureAwait(false);
        var crashDelivery = claimed.SingleOrDefault(static delivery => delivery.DedupeKey == CrashRecoveryDedupeKey);
        Check(checks, "crashReminderLeased", crashDelivery is { AttemptCount: 1 });

        var snapshot = await _inspectionService.InspectReminderAsync(
            CrashRecoveryDedupeKey,
            UiSmokeScenarioSeeder.ShanghaiEventId,
            cancellationToken).ConfigureAwait(false);
        Check(checks, "dedupeKeyCreatedExactlyOnce", snapshot.Outbox.Count == 1);
        Check(checks, "leasedOutboxMatchesClaim", crashDelivery is not null
            && snapshot.Outbox.SingleOrDefault() is { } row
            && row.OutboxId == crashDelivery.OutboxId
            && row.State == ReminderDeliveryState.Leased
            && row.AttemptCount == 1
            && row.LeaseUntil > now);
        Check(checks, "noCompletionLoggedBeforeCrash", snapshot.ReminderLogCount == 0);
        Check(checks, "acknowledgementPersistedBeforeCrash", snapshot.ActiveAcknowledgementCount == 1);
        Check(checks, "databaseIntegrityBeforeCrash", snapshot.IntegrityResult == "ok");
        return snapshot;
    }

    private async Task<ReminderPersistenceSnapshot> VerifyAsync(
        Dictionary<string, bool> checks,
        CancellationToken cancellationToken)
    {
        var acknowledged = await RequireEventAsync(UiSmokeScenarioSeeder.ShanghaiEventId, cancellationToken).ConfigureAwait(false);
        var shenzhen = await RequireEventAsync(UiSmokeScenarioSeeder.ShenzhenEventId, cancellationToken).ConfigureAwait(false);
        var beijing = await RequireEventAsync(UiSmokeScenarioSeeder.BeijingEventId, cancellationToken).ConfigureAwait(false);
        Check(checks, "acknowledgementSurvivesForcedTermination", acknowledged.LifecycleStatus == IpoLifecycleStatus.Acknowledged);
        Check(checks, "pendingTasksSurviveForcedTermination", IsPending(shenzhen) && IsPending(beijing));

        var before = await _inspectionService.InspectReminderAsync(
            CrashRecoveryDedupeKey,
            UiSmokeScenarioSeeder.ShanghaiEventId,
            cancellationToken).ConfigureAwait(false);
        var leasedBeforeRestart = before.Outbox.SingleOrDefault();
        var now = ChinaTime.Now(_timeProvider);
        Check(checks, "leasedReminderSurvivesForcedTermination", before.Outbox.Count == 1
            && leasedBeforeRestart is { State: ReminderDeliveryState.Leased, AttemptCount: 1 }
            && leasedBeforeRestart.LeaseUntil < now);

        var reclaimed = await _repository.ClaimDueRemindersAsync(now, LeaseDuration, 100, cancellationToken).ConfigureAwait(false);
        var crashDelivery = reclaimed.SingleOrDefault(static delivery => delivery.DedupeKey == CrashRecoveryDedupeKey);
        Check(checks, "expiredLeaseReclaimedAfterRestart", crashDelivery is { AttemptCount: 2 }
            && leasedBeforeRestart is not null
            && crashDelivery.OutboxId == leasedBeforeRestart.OutboxId);

        if (crashDelivery is not null)
        {
            await _repository.CompleteReminderAsync(crashDelivery.OutboxId, now, "process-smoke", cancellationToken).ConfigureAwait(false);
            await _repository.CompleteReminderAsync(crashDelivery.OutboxId, now.AddMilliseconds(1), "process-smoke-duplicate", cancellationToken).ConfigureAwait(false);
        }

        await _repository.EnqueueRemindersAsync(
            [new ReminderScheduleItem
            {
                IpoEventId = shenzhen.Id,
                EventVersion = shenzhen.EventVersion,
                DueAt = now,
                Level = ReminderLevel.DataChanged,
                DedupeKey = CrashRecoveryDedupeKey,
            }],
            cancellationToken).ConfigureAwait(false);
        var afterDuplicateEnqueue = await _repository.ClaimDueRemindersAsync(
            now.AddSeconds(1),
            LeaseDuration,
            100,
            cancellationToken).ConfigureAwait(false);
        Check(checks, "deliveredDedupeKeyIsNotReclaimed", afterDuplicateEnqueue.All(static delivery => delivery.DedupeKey != CrashRecoveryDedupeKey));

        var final = await _inspectionService.InspectReminderAsync(
            CrashRecoveryDedupeKey,
            UiSmokeScenarioSeeder.ShanghaiEventId,
            cancellationToken).ConfigureAwait(false);
        Check(checks, "dedupeKeyRemainsSingleAfterRestart", final.Outbox.Count == 1);
        Check(checks, "completionIsIdempotent", final.Outbox.SingleOrDefault() is { State: ReminderDeliveryState.Delivered, AttemptCount: 2 }
            && final.ReminderLogCount == 1);
        Check(checks, "acknowledgementRowRemainsSingle", final.ActiveAcknowledgementCount == 1);
        Check(checks, "databaseIntegrityAfterRecovery", final.IntegrityResult == "ok");
        return final;
    }

    private async Task<IpoEvent> RequireEventAsync(string eventId, CancellationToken cancellationToken) =>
        await _repository.GetEventAsync(eventId, cancellationToken).ConfigureAwait(false)
        ?? throw new InvalidOperationException($"进程恢复 smoke 任务不存在：{eventId}");

    private static bool IsPending(IpoEvent ipoEvent) => ipoEvent.LifecycleStatus is
        IpoLifecycleStatus.Scheduled or IpoLifecycleStatus.ActiveUnconfirmed or IpoLifecycleStatus.AcknowledgedNeedsReview;

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
}
