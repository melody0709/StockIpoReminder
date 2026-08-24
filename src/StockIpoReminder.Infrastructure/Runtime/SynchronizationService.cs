using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Announcements;
using StockIpoReminder.Infrastructure.Operations;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed record SynchronizationSummary
{
    public bool Success { get; init; }
    public int SuccessfulSources { get; init; }
    public int FailedSources { get; init; }
    public int DeferredSources { get; init; }
    public int CandidateCount { get; init; }
    public int EventCount { get; init; }
    public int AnnouncementCount { get; init; }
    public string? Error { get; init; }
}

public sealed class SynchronizationService : IDisposable
{
    private readonly IReadOnlyList<IIpoCollector> _collectors;
    private readonly IReadOnlyList<IAnnouncementProvider> _announcementProviders;
    private readonly IAnnouncementProcessor _announcementProcessor;
    private readonly IIpoRepository _repository;
    private readonly IpoReconciler _reconciler;
    private readonly ReminderPlanner _planner;
    private readonly RuntimeState _runtimeState;
    private readonly TimeProvider _timeProvider;
    private readonly ILogger<SynchronizationService> _logger;
    private readonly SourceBackoffStore? _sourceBackoffStore;
    private readonly SemaphoreSlim _gate = new(1, 1);

    public SynchronizationService(
        IEnumerable<IIpoCollector> collectors,
        IEnumerable<IAnnouncementProvider> announcementProviders,
        IAnnouncementProcessor announcementProcessor,
        IIpoRepository repository,
        IpoReconciler reconciler,
        ReminderPlanner planner,
        RuntimeState runtimeState,
        TimeProvider timeProvider,
        ILogger<SynchronizationService> logger,
        SourceBackoffStore? sourceBackoffStore = null)
    {
        _collectors = collectors.ToArray();
        _announcementProviders = announcementProviders.ToArray();
        _announcementProcessor = announcementProcessor;
        _repository = repository;
        _reconciler = reconciler;
        _planner = planner;
        _runtimeState = runtimeState;
        _timeProvider = timeProvider;
        _logger = logger;
        _sourceBackoffStore = sourceBackoffStore;
    }

    public async Task<SynchronizationSummary> SynchronizeAsync(string reason, CancellationToken cancellationToken)
    {
        if (!await _gate.WaitAsync(0, cancellationToken).ConfigureAwait(false))
        {
            return new SynchronizationSummary { Success = true };
        }

        var started = ChinaTime.Now(_timeProvider);
        _runtimeState.Update(snapshot => snapshot with
        {
            IsSynchronizing = true,
            LastSyncStartedAt = started,
            StatusText = $"正在同步（{reason}）",
            LastError = null,
        });

        try
        {
            var settings = await _repository.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
            var tasks = _collectors.Select(collector => CollectSafelyAsync(collector, cancellationToken)).ToArray();
            var results = await Task.WhenAll(tasks).ConfigureAwait(false);
            foreach (var result in results.Where(static result => !result.IsDeferred))
            {
                await _repository.SaveCollectorResultAsync(result, cancellationToken).ConfigureAwait(false);
            }

            var successful = results.Where(static x => x.Success).ToArray();
            if (successful.Length == 0)
            {
                throw new InvalidOperationException("所有新股数据源均同步失败，已保留上一次有效数据。" );
            }

            var today = ChinaTime.Today(_timeProvider);
            var candidates = successful
                .SelectMany(static result => result.Candidates)
                .Where(candidate => settings.IsExchangeEnabled(candidate.Exchange))
                .Where(candidate => candidate.ApplyDate is null
                    || candidate.ApplyDate >= today.AddDays(-30)
                    || candidate.Status is IssueStatus.Upcoming or IssueStatus.Active)
                .Where(static candidate => candidate.StableIdentity is not null)
                .ToArray();
            var groups = candidates.GroupBy(static candidate => candidate.StableIdentity!, StringComparer.OrdinalIgnoreCase).ToArray();
            var eventCount = 0;
            var announcementCount = 0;

            foreach (var group in groups)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var existing = await _repository.GetPublicEventAsync(group.Key, cancellationToken).ConfigureAwait(false);
                var provisional = _reconciler.Reconcile(group.ToArray(), existing, settings, started);
                if (provisional is null)
                {
                    continue;
                }

                var documents = new List<AnnouncementDocument>();
                var announcementCandidates = new List<IpoCandidate>();
                var applicableAnnouncementProviderSeen = false;
                var announcementChainUnavailable = false;
                var hasUsableAnnouncementEvidence = false;
                if (ShouldCheckAnnouncements(provisional.Event, today))
                {
                    var existingDocuments = existing is null
                        ? []
                        : await _repository.GetAnnouncementsAsync(existing.Id, cancellationToken).ConfigureAwait(false);
                    hasUsableAnnouncementEvidence = existingDocuments.Any(HasUsableAnnouncementEvidence);

                    foreach (var provider in _announcementProviders.Where(provider => provider.Supports(provisional.Event.Exchange)))
                    {
                        applicableAnnouncementProviderSeen = true;
                        var providerStarted = ChinaTime.Now(_timeProvider);
                        if (_sourceBackoffStore is not null)
                        {
                            var decision = await _sourceBackoffStore.GetDecisionAsync(
                                provider.ProviderName,
                                providerStarted,
                                cancellationToken).ConfigureAwait(false);
                            if (!decision.CanAttempt)
                            {
                                _logger.LogInformation(
                                    "公告源 {Provider} 处于退避状态，下一次尝试 {NextAttemptAt}",
                                    provider.ProviderName,
                                    decision.NextAttemptAt);
                                announcementChainUnavailable = true;
                                continue;
                            }
                        }

                        try
                        {
                            var references = await provider.SearchAsync(
                                provisional.Event,
                                today.AddDays(-60),
                                today.AddDays(1),
                                cancellationToken).ConfigureAwait(false);
                            var processingErrors = new List<Exception>();

                            foreach (var reference in references
                                .OrderByDescending(static reference => reference.PublishedAt)
                                .Take(12))
                            {
                                try
                                {
                                    var document = await _announcementProcessor.DownloadAndParseAsync(
                                        provisional.Event,
                                        reference,
                                        cancellationToken).ConfigureAwait(false);
                                    documents.Add(document);
                                    if (document.ExtractionStatus is ExtractionStatus.Failed or ExtractionStatus.Unsupported)
                                    {
                                        processingErrors.Add(new InvalidDataException(
                                            $"公告 {reference.Provider}/{reference.AnnouncementId} 的文本提取状态为 {document.ExtractionStatus}。"));
                                        announcementChainUnavailable = true;
                                    }

                                    if (document.ParsedFields.Any(static field => field.Confidence >= 0.90m))
                                    {
                                        announcementCandidates.Add(AnnouncementCandidateFactory.Create(provisional.Event, document));
                                        hasUsableAnnouncementEvidence |= document.ExtractionStatus == ExtractionStatus.Extracted;
                                    }
                                }
                                catch (Exception ex) when (ex is not OperationCanceledException)
                                {
                                    processingErrors.Add(ex);
                                    announcementChainUnavailable = true;
                                    _logger.LogWarning(ex, "无法下载或解析公告 {Provider}/{AnnouncementId}", reference.Provider, reference.AnnouncementId);
                                    var cachedDocument = existingDocuments
                                        .Where(document => document.Reference.Provider == reference.Provider
                                            && document.Reference.AnnouncementId == reference.AnnouncementId)
                                        .OrderByDescending(static document => document.DownloadedAt)
                                        .FirstOrDefault();
                                    if (cachedDocument?.ParsedFields.Any(static field => field.Confidence >= 0.90m) == true)
                                    {
                                        announcementCandidates.Add(AnnouncementCandidateFactory.Create(provisional.Event, cachedDocument));
                                        hasUsableAnnouncementEvidence |= cachedDocument.ExtractionStatus == ExtractionStatus.Extracted;
                                    }
                                }
                            }

                            var providerFinished = ChinaTime.Now(_timeProvider);
                            if (processingErrors.Count == 0)
                            {
                                await _repository.SaveCollectorResultAsync(new CollectorResult
                                {
                                    Source = provider.ProviderName,
                                    Success = true,
                                    StartedAt = providerStarted,
                                    FinishedAt = providerFinished,
                                    RecordCount = references.Count,
                                }, cancellationToken).ConfigureAwait(false);
                                if (_sourceBackoffStore is not null)
                                {
                                    await _sourceBackoffStore.RecordSuccessAsync(
                                        provider.ProviderName,
                                        providerFinished,
                                        cancellationToken).ConfigureAwait(false);
                                }
                            }
                            else
                            {
                                var firstError = DiagnosticRedactor.Redact(processingErrors[0].Message);
                                var aggregate = new InvalidDataException(
                                    $"公告源 {provider.ProviderName} 有 {processingErrors.Count}/{references.Count} 个文档下载或解析失败；首个错误：{firstError}");
                                var failed = CollectorResult.Failed(
                                    provider.ProviderName,
                                    providerStarted,
                                    providerFinished,
                                    aggregate);
                                await _repository.SaveCollectorResultAsync(failed, cancellationToken).ConfigureAwait(false);
                                if (_sourceBackoffStore is not null)
                                {
                                    await _sourceBackoffStore.RecordFailureAsync(
                                        provider.ProviderName,
                                        failed.FinishedAt,
                                        failed.RetryAfter,
                                        failed.Error,
                                        cancellationToken).ConfigureAwait(false);
                                }
                            }
                        }
                        catch (Exception ex) when (ex is not OperationCanceledException)
                        {
                            announcementChainUnavailable = true;
                            _logger.LogWarning(ex, "公告源 {Provider} 查询失败", provider.ProviderName);
                            var failed = CollectorResult.Failed(
                                provider.ProviderName,
                                providerStarted,
                                ChinaTime.Now(_timeProvider),
                                ex);
                            await _repository.SaveCollectorResultAsync(failed, cancellationToken).ConfigureAwait(false);
                            if (_sourceBackoffStore is not null)
                            {
                                await _sourceBackoffStore.RecordFailureAsync(
                                    provider.ProviderName,
                                    failed.FinishedAt,
                                    failed.RetryAfter,
                                    failed.Error,
                                    cancellationToken).ConfigureAwait(false);
                            }
                        }
                    }
                }

                var combined = group.Concat(announcementCandidates).ToArray();
                var resolved = _reconciler.Reconcile(combined, existing, settings, ChinaTime.Now(_timeProvider)) ?? provisional;
                var incompleteDocument = documents.Any(static document =>
                    document.ExtractionStatus is ExtractionStatus.Failed or ExtractionStatus.Unsupported or ExtractionStatus.LowConfidence);
                var missingNearTermEvidence = RequiresOfficialAnnouncementEvidence(provisional.Event, today)
                    && !hasUsableAnnouncementEvidence;
                if (applicableAnnouncementProviderSeen
                    && !hasUsableAnnouncementEvidence
                    && (announcementChainUnavailable || incompleteDocument || missingNearTermEvidence))
                {
                    resolved = resolved with
                    {
                        Event = resolved.Event with { DataQualityStatus = DataQualityStatus.ManualReviewRequired },
                    };
                }

                var upsert = await _repository.UpsertEventAsync(resolved, cancellationToken).ConfigureAwait(false);
                foreach (var document in documents)
                {
                    await _repository.SaveAnnouncementAsync(document with { IpoEventId = upsert.Event.Id }, cancellationToken).ConfigureAwait(false);
                    announcementCount++;
                }

                var effectiveEvent = await _repository.GetEventAsync(upsert.Event.Id, cancellationToken).ConfigureAwait(false)
                    ?? upsert.Event;
                await _repository.ReconcileReminderScheduleAsync(
                    effectiveEvent.Id,
                    effectiveEvent.EventVersion,
                    _planner.Plan(effectiveEvent, settings),
                    ChinaTime.Now(_timeProvider),
                    cancellationToken).ConfigureAwait(false);
                eventCount++;
            }

            var completed = ChinaTime.Now(_timeProvider);
            _runtimeState.Update(snapshot => snapshot with
            {
                IsSynchronizing = false,
                LastSyncCompletedAt = completed,
                LastSyncSucceeded = true,
                LastCandidateCount = candidates.Length,
                LastEventCount = eventCount,
                StatusText = $"同步完成：{eventCount} 个发行任务",
            });
            return new SynchronizationSummary
            {
                Success = true,
                SuccessfulSources = successful.Length,
                FailedSources = results.Count(static result => !result.Success && !result.IsDeferred),
                DeferredSources = results.Count(static result => result.IsDeferred),
                CandidateCount = candidates.Length,
                EventCount = eventCount,
                AnnouncementCount = announcementCount,
            };
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            _logger.LogError(ex, "同步失败，原因：{Reason}", reason);
            _runtimeState.Update(snapshot => snapshot with
            {
                IsSynchronizing = false,
                LastSyncCompletedAt = ChinaTime.Now(_timeProvider),
                LastSyncSucceeded = false,
                StatusText = "同步失败，继续使用缓存数据",
                LastError = ex.Message,
            });
            return new SynchronizationSummary { Success = false, Error = ex.Message };
        }
        finally
        {
            _gate.Release();
        }
    }

    private static bool ShouldCheckAnnouncements(IpoEvent ipoEvent, DateOnly today) =>
        ipoEvent.ApplyDate is not null
        && ipoEvent.ApplyDate >= today.AddDays(-7)
        && ipoEvent.ApplyDate <= today.AddDays(45);

    private static bool RequiresOfficialAnnouncementEvidence(IpoEvent ipoEvent, DateOnly today) =>
        ipoEvent.ApplyDate is { } applyDate
        && applyDate >= today.AddDays(-7)
        && applyDate <= today.AddDays(7)
        && ipoEvent.Status is IssueStatus.Upcoming or IssueStatus.Active;

    private static bool HasUsableAnnouncementEvidence(AnnouncementDocument document) =>
        document.ExtractionStatus == ExtractionStatus.Extracted
        && document.ParsedFields.Any(static field => field.Confidence >= 0.90m);

    private async Task<CollectorResult> CollectSafelyAsync(IIpoCollector collector, CancellationToken cancellationToken)
    {
        var started = ChinaTime.Now(_timeProvider);
        if (_sourceBackoffStore is not null)
        {
            var decision = await _sourceBackoffStore.GetDecisionAsync(
                collector.SourceName,
                started,
                cancellationToken).ConfigureAwait(false);
            if (!decision.CanAttempt && decision.NextAttemptAt is { } nextAttempt)
            {
                return CollectorResult.Deferred(collector.SourceName, started, nextAttempt);
            }
        }

        CollectorResult result;
        try
        {
            result = await collector.CollectAsync(cancellationToken).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            throw;
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "新股数据源 {Source} 未返回标准失败结果", collector.SourceName);
            result = CollectorResult.Failed(collector.SourceName, started, ChinaTime.Now(_timeProvider), ex);
        }

        if (_sourceBackoffStore is not null)
        {
            if (result.Success)
            {
                await _sourceBackoffStore.RecordSuccessAsync(
                    collector.SourceName,
                    result.FinishedAt,
                    cancellationToken).ConfigureAwait(false);
            }
            else
            {
                await _sourceBackoffStore.RecordFailureAsync(
                    collector.SourceName,
                    result.FinishedAt,
                    result.RetryAfter,
                    result.Error,
                    cancellationToken).ConfigureAwait(false);
            }
        }

        return result;
    }

    public void Dispose() => _gate.Dispose();
}
