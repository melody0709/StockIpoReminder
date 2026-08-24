using System.Globalization;
using System.Text.RegularExpressions;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class ReminderManagementService
{
    private readonly IIpoRepository _repository;
    private readonly ReminderPlanner _planner;
    private readonly ISyncTrigger _syncTrigger;
    private readonly TimeProvider _timeProvider;

    public ReminderManagementService(
        IIpoRepository repository,
        ReminderPlanner planner,
        ISyncTrigger syncTrigger,
        TimeProvider timeProvider)
    {
        _repository = repository;
        _planner = planner;
        _syncTrigger = syncTrigger;
        _timeProvider = timeProvider;
    }

    public Task<IReadOnlyList<IpoEvent>> GetEventsAsync(DateOnly from, DateOnly to, CancellationToken cancellationToken = default) =>
        _repository.GetEventsAsync(from, to, cancellationToken);

    public Task<HealthSummary> GetHealthSummaryAsync(CancellationToken cancellationToken = default)
    {
        var now = ChinaTime.Now(_timeProvider);
        return _repository.GetHealthSummaryAsync(DateOnly.FromDateTime(now.DateTime), now, cancellationToken);
    }

    public Task<AppSettings> GetSettingsAsync(CancellationToken cancellationToken = default) => _repository.GetSettingsAsync(cancellationToken);

    public async Task<IpoEventDetails> GetEventDetailsAsync(string eventId, CancellationToken cancellationToken = default)
    {
        var ipoEvent = await _repository.GetEventAsync(eventId, cancellationToken).ConfigureAwait(false)
            ?? throw new InvalidOperationException("申购任务不存在或已经被替换。" );
        var sourcesTask = _repository.GetFieldSourcesAsync(eventId, cancellationToken);
        var announcementsTask = _repository.GetAnnouncementsAsync(eventId, cancellationToken);
        var overridesTask = _repository.GetManualOverridesAsync(eventId, ipoEvent.EventVersion, cancellationToken);
        await Task.WhenAll(sourcesTask, announcementsTask, overridesTask).ConfigureAwait(false);
        return new IpoEventDetails
        {
            Event = ipoEvent,
            FieldSources = sourcesTask.Result,
            Announcements = announcementsTask.Result,
            ManualOverrides = overridesTask.Result,
        };
    }

    public async Task SaveSettingsAsync(AppSettings settings, CancellationToken cancellationToken = default)
    {
        await _repository.SaveSettingsAsync(settings, cancellationToken).ConfigureAwait(false);
        var today = ChinaTime.Today(_timeProvider);
        var events = await _repository.GetEventsAsync(today.AddDays(-1), today.AddDays(60), cancellationToken).ConfigureAwait(false);
        foreach (var ipoEvent in events)
        {
            await _repository.ReconcileReminderScheduleAsync(
                ipoEvent.Id,
                ipoEvent.EventVersion,
                _planner.Plan(ipoEvent, settings),
                ChinaTime.Now(_timeProvider),
                cancellationToken).ConfigureAwait(false);
        }

        _syncTrigger.RequestSync("设置变更");
    }

    public async Task AcknowledgeAsync(string eventId, int eventVersion, CancellationToken cancellationToken = default)
    {
        var ipoEvent = await _repository.GetEventAsync(eventId, cancellationToken).ConfigureAwait(false)
            ?? throw new InvalidOperationException("申购任务不存在或已经被替换。" );
        if (ipoEvent.EventVersion != eventVersion)
        {
            throw new InvalidOperationException("申购数据已经更新，请重新查看后确认。" );
        }

        await _repository.AcknowledgeAsync(
            eventId,
            eventVersion,
            ChinaTime.Now(_timeProvider),
            EventDataHasher.Compute(ipoEvent),
            cancellationToken).ConfigureAwait(false);
    }

    public async Task RevokeAcknowledgementAsync(string eventId, int eventVersion, CancellationToken cancellationToken = default)
    {
        var now = ChinaTime.Now(_timeProvider);
        var ipoEvent = await _repository.GetEventAsync(eventId, cancellationToken).ConfigureAwait(false)
            ?? throw new InvalidOperationException("申购任务不存在。" );
        if (ipoEvent.EventVersion != eventVersion)
        {
            throw new InvalidOperationException("申购数据已经更新，不能撤销旧版本的确认。" );
        }

        if (ipoEvent.LifecycleStatus != IpoLifecycleStatus.Acknowledged)
        {
            throw new InvalidOperationException("该申购任务当前没有可撤销的有效确认。" );
        }

        var settings = await _repository.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
        var cutoff = ReminderPlanner.GetEffectiveSafetyCutoff(ipoEvent, settings);
        if (ipoEvent.ApplyDate is null || now >= ChinaTime.At(ipoEvent.ApplyDate.Value, cutoff))
        {
            throw new InvalidOperationException("已超过安全截止时间，不能撤销确认。" );
        }

        await _repository.RevokeAcknowledgementAsync(eventId, eventVersion, now, cancellationToken).ConfigureAwait(false);
        var current = ipoEvent with { LifecycleStatus = IpoLifecycleStatus.ActiveUnconfirmed };
        await _repository.ReconcileReminderScheduleAsync(
            current.Id,
            current.EventVersion,
            _planner.Plan(current, settings).Where(item => item.DueAt >= now).ToArray(),
            now,
            cancellationToken).ConfigureAwait(false);
    }

    public async Task ApplyManualOverrideAsync(
        string eventId,
        int eventVersion,
        string fieldName,
        string value,
        string reason,
        string? announcementDocumentId,
        CancellationToken cancellationToken = default)
    {
        var ipoEvent = await _repository.GetEventAsync(eventId, cancellationToken).ConfigureAwait(false)
            ?? throw new InvalidOperationException("申购任务不存在。" );
        if (ipoEvent.EventVersion != eventVersion)
        {
            throw new InvalidOperationException("申购任务数据版本已经变化，请刷新详情后重试。" );
        }

        var normalized = NormalizeManualOverride(fieldName, value);
        if (string.IsNullOrWhiteSpace(reason))
        {
            throw new InvalidOperationException("人工覆盖必须填写核验理由。" );
        }

        await _repository.AddManualOverrideAsync(
            eventId,
            eventVersion,
            fieldName,
            normalized,
            reason.Trim(),
            announcementDocumentId,
            cancellationToken).ConfigureAwait(false);
        await ReplanEffectiveEventAsync(eventId, cancellationToken).ConfigureAwait(false);
    }

    public async Task RevokeManualOverrideAsync(
        string eventId,
        int eventVersion,
        long overrideId,
        CancellationToken cancellationToken = default)
    {
        var records = await _repository.GetManualOverridesAsync(eventId, eventVersion, cancellationToken).ConfigureAwait(false);
        if (!records.Any(item => item.Id == overrideId && item.RevokedAt is null))
        {
            throw new InvalidOperationException("人工覆盖记录不存在、已经撤销或属于旧的数据版本。" );
        }

        await _repository.RevokeManualOverrideAsync(overrideId, ChinaTime.Now(_timeProvider), cancellationToken).ConfigureAwait(false);
        await ReplanEffectiveEventAsync(eventId, cancellationToken).ConfigureAwait(false);
    }

    public void RequestSync(string reason = "用户手动") => _syncTrigger.RequestSync(reason);

    private async Task ReplanEffectiveEventAsync(string eventId, CancellationToken cancellationToken)
    {
        var effective = await _repository.GetEventAsync(eventId, cancellationToken).ConfigureAwait(false)
            ?? throw new InvalidOperationException("申购任务不存在。" );
        var settings = await _repository.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
        await _repository.ReconcileReminderScheduleAsync(
            effective.Id,
            effective.EventVersion,
            _planner.Plan(effective, settings),
            ChinaTime.Now(_timeProvider),
            cancellationToken).ConfigureAwait(false);
    }

    private static string NormalizeManualOverride(string fieldName, string value)
    {
        var trimmed = value.Trim();
        return fieldName switch
        {
            "ApplyCode" when Regex.IsMatch(trimmed, "^\\d{6}$", RegexOptions.CultureInvariant) => trimmed,
            "ApplyDate" when ValueNormalizer.Date(trimmed) is { } date => date.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            "IssuePrice" when ValueNormalizer.Decimal(trimmed, zeroMeansMissing: true) is { } price => price.ToString(CultureInfo.InvariantCulture),
            "LotSize" when ValueNormalizer.Integer(trimmed, zeroMeansMissing: true) is { } lot => lot.ToString(CultureInfo.InvariantCulture),
            "MaxApplyQuantity" when ValueNormalizer.Integer(trimmed, zeroMeansMissing: true) is { } maximum => maximum.ToString(CultureInfo.InvariantCulture),
            "OfficialSessions" when IsValidSessionText(trimmed) => trimmed.Replace('，', ',').Replace('；', ','),
            "IssueStatus" when TryNormalizeStatus(trimmed, out var status) => status,
            "ApplyCode" => throw new InvalidOperationException("申购代码必须是 6 位数字。" ),
            "ApplyDate" => throw new InvalidOperationException("申购日期格式无效，请使用 yyyy-MM-dd。" ),
            "IssuePrice" => throw new InvalidOperationException("发行价格必须是大于 0 的数字。" ),
            "LotSize" => throw new InvalidOperationException("申购单位必须是大于 0 的整数股数。" ),
            "MaxApplyQuantity" => throw new InvalidOperationException("申购上限必须是大于 0 的整数股数。" ),
            "OfficialSessions" => throw new InvalidOperationException("官方时段格式无效，例如 09:30-11:30,13:00-15:00。" ),
            "IssueStatus" => throw new InvalidOperationException("发行状态无效。" ),
            _ => throw new InvalidOperationException("该字段不允许人工覆盖。" ),
        };
    }

    private static bool IsValidSessionText(string value)
    {
        var pairs = value.Split([',', '，', ';', '；'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (pairs.Length is < 1 or > 3)
        {
            return false;
        }

        foreach (var pair in pairs)
        {
            var normalized = pair.Replace('—', '-').Replace('–', '-').Replace("至", "-", StringComparison.Ordinal);
            var bounds = normalized.Split('-', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            if (bounds.Length != 2
                || !TimeOnly.TryParseExact(bounds[0], ["H:mm", "HH:mm"], CultureInfo.InvariantCulture, DateTimeStyles.None, out var start)
                || !TimeOnly.TryParseExact(bounds[1], ["H:mm", "HH:mm"], CultureInfo.InvariantCulture, DateTimeStyles.None, out var end)
                || start >= end)
            {
                return false;
            }
        }

        return true;
    }

    private static bool TryNormalizeStatus(string value, out string normalized)
    {
        normalized = value switch
        {
            "即将发行" or "正常发行" or "Upcoming" => IssueStatus.Upcoming.ToString(),
            "申购中" or "Active" => IssueStatus.Active.ToString(),
            "延期发行" or "暂缓发行" or "Postponed" => IssueStatus.Postponed.ToString(),
            "中止发行" or "Suspended" => IssueStatus.Suspended.ToString(),
            "终止发行" or "Terminated" => IssueStatus.Terminated.ToString(),
            "发行完成" or "Completed" => IssueStatus.Completed.ToString(),
            _ => string.Empty,
        };
        return normalized.Length > 0;
    }
}
