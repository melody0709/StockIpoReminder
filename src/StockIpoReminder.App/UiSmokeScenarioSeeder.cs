using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class UiSmokeScenarioSeeder
{
    public const string ShanghaiEventId = "shanghai:688001";
    public const string ShenzhenEventId = "shenzhen:301001";
    public const string BeijingEventId = "beijing:920001";
    public const string FuturePostponedEventId = "shanghai:600101";
    public const string FutureSuspendedEventId = "shenzhen:001101";
    public const string FutureTerminatedEventId = "beijing:920101";
    public const string RescheduledReviewEventId = "shanghai:600102";
    public const string ShanghaiAnnouncementDocumentId = "ui-smoke:ui-smoke-sse-688001";

    private readonly IIpoRepository _repository;
    private readonly ReminderPlanner _planner;
    private readonly TimeProvider _timeProvider;
    private readonly ApplicationRuntimeOptions _runtimeOptions;

    public UiSmokeScenarioSeeder(
        IIpoRepository repository,
        ReminderPlanner planner,
        TimeProvider timeProvider,
        ApplicationRuntimeOptions runtimeOptions)
    {
        _repository = repository;
        _planner = planner;
        _timeProvider = timeProvider;
        _runtimeOptions = runtimeOptions;
    }

    public async Task SeedAsync(CancellationToken cancellationToken = default)
    {
        if (!_runtimeOptions.SmokeMode || !_runtimeOptions.SmokeSeedScenarios)
        {
            throw new InvalidOperationException("确定性 UI 样本只能在显式 smoke 模式下写入隔离数据目录。");
        }

        var now = ChinaTime.Now(_timeProvider);
        var today = DateOnly.FromDateTime(now.DateTime);
        var settings = new AppSettings
        {
            ShanghaiEnabled = true,
            ShenzhenEnabled = true,
            BeijingEnabled = true,
            SafetyCutoff = new TimeOnly(14, 55),
            SoundEnabled = false,
            FlashTaskbar = false,
            ToastEnabled = true,
            DailyHealthSummaryEnabled = false,
            AutoStartEnabled = false,
            NotificationSelfTestCompleted = true,
            OnboardingCompleted = true,
        };
        await _repository.SaveSettingsAsync(settings, cancellationToken).ConfigureAwait(false);

        var shanghai = Event(
            ShanghaiEventId,
            Exchange.Shanghai,
            Board.Star,
            "688001",
            "787001",
            "沪测科技",
            today,
            18.88m,
            500,
            12_500,
            DataQualityStatus.AnnouncementVerified,
            Sessions(Exchange.Shanghai, settings.SafetyCutoff),
            now,
            "https://www.sse.com.cn/ui-smoke/688001.pdf");
        var shenzhen = Event(
            ShenzhenEventId,
            Exchange.Shenzhen,
            Board.ChiNext,
            "301001",
            applyCode: null,
            "深测股份",
            today,
            issuePrice: null,
            lotSize: 500,
            maximum: null,
            DataQualityStatus.ManualReviewRequired,
            Sessions(Exchange.Shenzhen, settings.SafetyCutoff),
            now,
            "https://www.cninfo.com.cn/ui-smoke/301001.pdf");
        var beijing = Event(
            BeijingEventId,
            Exchange.Beijing,
            Board.Beijing,
            "920001",
            "920001",
            "北测创新",
            today,
            11.80m,
            100,
            200_000,
            DataQualityStatus.ManualReviewRequired,
            Sessions(Exchange.Beijing, settings.SafetyCutoff),
            now,
            "https://www.bseinfo.net/disclosure/ui-smoke/920001.pdf") with
        {
            RequiredCash = 2_360_000m,
        };

        var seededShanghai = await UpsertAsync(
            shanghai,
            FieldsFor(shanghai, "sse-announcement", 100, now),
            settings,
            now,
            cancellationToken).ConfigureAwait(false);
        await UpsertAsync(
            shenzhen,
            FieldsFor(shenzhen, "cninfo", 80, now),
            settings,
            now,
            cancellationToken).ConfigureAwait(false);
        var seededBeijing = await UpsertAsync(
            beijing,
            FieldsFor(beijing, "bse-announcement", 100, now),
            settings,
            now,
            cancellationToken).ConfigureAwait(false);

        var futureDate = today.AddDays(1);
        foreach (var terminalEvent in new[]
        {
            Event(
                FuturePostponedEventId,
                Exchange.Shanghai,
                Board.Main,
                "600101",
                "730101",
                "延期样本",
                futureDate,
                9.80m,
                1000,
                20_000,
                DataQualityStatus.AnnouncementVerified,
                Sessions(Exchange.Shanghai, settings.SafetyCutoff),
                now,
                "https://www.sse.com.cn/ui-smoke/600101.pdf",
                IssueStatus.Postponed,
                IpoLifecycleStatus.SuspendedOrCancelled),
            Event(
                FutureSuspendedEventId,
                Exchange.Shenzhen,
                Board.Main,
                "001101",
                "001101",
                "暂缓样本",
                futureDate,
                8.60m,
                500,
                15_000,
                DataQualityStatus.AnnouncementVerified,
                Sessions(Exchange.Shenzhen, settings.SafetyCutoff),
                now,
                "https://www.cninfo.com.cn/ui-smoke/001101.pdf",
                IssueStatus.Suspended,
                IpoLifecycleStatus.SuspendedOrCancelled),
            Event(
                FutureTerminatedEventId,
                Exchange.Beijing,
                Board.Beijing,
                "920101",
                "920101",
                "终止样本",
                futureDate,
                7.50m,
                100,
                50_000,
                DataQualityStatus.AnnouncementVerified,
                Sessions(Exchange.Beijing, settings.SafetyCutoff),
                now,
                "https://www.bseinfo.net/disclosure/ui-smoke/920101.pdf",
                IssueStatus.Terminated,
                IpoLifecycleStatus.SuspendedOrCancelled),
        })
        {
            await UpsertAsync(
                terminalEvent,
                FieldsFor(terminalEvent, "ui-smoke-announcement", 100, now),
                settings,
                now,
                cancellationToken).ConfigureAwait(false);
        }

        var reviewOriginal = Event(
            RescheduledReviewEventId,
            Exchange.Shanghai,
            Board.Main,
            "600102",
            "730102",
            "改期重确认样本",
            today.AddDays(2),
            10.20m,
            1000,
            18_000,
            DataQualityStatus.AnnouncementVerified,
            Sessions(Exchange.Shanghai, settings.SafetyCutoff),
            now,
            "https://www.sse.com.cn/ui-smoke/600102.pdf",
            IssueStatus.Active,
            IpoLifecycleStatus.Scheduled);
        var acknowledgedReview = await UpsertAsync(
            reviewOriginal,
            FieldsFor(reviewOriginal, "sse-announcement", 100, now),
            settings,
            now,
            cancellationToken).ConfigureAwait(false);
        await _repository.AcknowledgeAsync(
            acknowledgedReview.Id,
            acknowledgedReview.EventVersion,
            now,
            EventDataHasher.Compute(acknowledgedReview),
            cancellationToken).ConfigureAwait(false);
        var rescheduled = reviewOriginal with
        {
            ApplyDate = futureDate,
            UpdatedAt = now.AddSeconds(1),
        };
        var needsReview = await UpsertAsync(
            rescheduled,
            FieldsFor(rescheduled, "sse-announcement", 100, now.AddSeconds(1)),
            settings,
            now.AddSeconds(1),
            cancellationToken).ConfigureAwait(false);
        if (needsReview.EventVersion < 2 || needsReview.LifecycleStatus != IpoLifecycleStatus.AcknowledgedNeedsReview)
        {
            throw new InvalidOperationException("UI smoke 改期样本没有进入需重新确认状态。");
        }

        await SaveAnnouncementAsync(
            seededShanghai,
            "sse-announcement",
            "ui-smoke-sse-688001",
            "沪测科技首次公开发行股票并在科创板上市发行公告",
            new Uri("https://www.sse.com.cn/ui-smoke/688001.pdf"),
            [
                Parsed("ApplyCode", "787001", "发行公告明确申购代码为 787001"),
                Parsed("IssuePrice", "18.88", "发行价格为每股 18.88 元"),
                Parsed("ApplyDate", today.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture), "网上申购日为本测试日期"),
            ],
            now,
            cancellationToken).ConfigureAwait(false);
        await SaveAnnouncementAsync(
            seededBeijing,
            "bse-announcement",
            "ui-smoke-bse-920001",
            "北测创新向不特定合格投资者公开发行股票并在北京证券交易所上市发行公告",
            new Uri("https://www.bseinfo.net/disclosure/ui-smoke/920001.pdf"),
            [
                Parsed("ApplyCode", "920001", "证券代码和申购代码均为 920001"),
                Parsed("FundingMode", "FullCash", "申购时需全额缴付申购资金"),
            ],
            now,
            cancellationToken).ConfigureAwait(false);

        foreach (var (source, count) in new[] { ("eastmoney", 7), ("sse", 3), ("cninfo", 2), ("bse", 2) })
        {
            await _repository.SaveCollectorResultAsync(new CollectorResult
            {
                Source = source,
                Success = true,
                StartedAt = now.AddSeconds(-2),
                FinishedAt = now.AddSeconds(-1),
                RecordCount = count,
                RawHash = ValueNormalizer.Sha256($"ui-smoke:{source}:{today:yyyy-MM-dd}"),
                SchemaFingerprint = "ui-smoke-v1",
            }, cancellationToken).ConfigureAwait(false);
        }

        await _repository.TouchHeartbeatAsync("scheduler", now, cancellationToken).ConfigureAwait(false);
        await _repository.TouchHeartbeatAsync("delivery", now, cancellationToken).ConfigureAwait(false);
    }

    private async Task<IpoEvent> UpsertAsync(
        IpoEvent ipoEvent,
        IReadOnlyList<SourceFieldValue> fields,
        AppSettings settings,
        DateTimeOffset now,
        CancellationToken cancellationToken)
    {
        var result = await _repository.UpsertEventAsync(new ReconciledIpoEvent
        {
            Event = ipoEvent,
            FieldSources = fields,
        }, cancellationToken).ConfigureAwait(false);
        await _repository.ReconcileReminderScheduleAsync(
            result.Event.Id,
            result.Event.EventVersion,
            _planner.Plan(result.Event, settings),
            now,
            cancellationToken).ConfigureAwait(false);
        return result.Event;
    }

    private async Task SaveAnnouncementAsync(
        IpoEvent ipoEvent,
        string provider,
        string announcementId,
        string title,
        Uri url,
        IReadOnlyList<ParsedAnnouncementField> fields,
        DateTimeOffset now,
        CancellationToken cancellationToken)
    {
        var directory = Path.Combine(_runtimeOptions.DataRoot, "announcements", "ui-smoke");
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, $"{announcementId}.pdf");
        var bytes = Encoding.ASCII.GetBytes(
            "%PDF-1.4\n1 0 obj<< /Type /Catalog >>endobj\n" +
            $"% deterministic UI smoke evidence for {ipoEvent.SecurityCode}\n%%EOF\n");
        await File.WriteAllBytesAsync(path, bytes, cancellationToken).ConfigureAwait(false);
        var hash = Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
        await _repository.SaveAnnouncementAsync(new AnnouncementDocument
        {
            Id = $"ui-smoke:{announcementId}",
            IpoEventId = ipoEvent.Id,
            Reference = new AnnouncementReference
            {
                Provider = provider,
                AnnouncementId = announcementId,
                AnnouncementType = "发行公告",
                Title = title,
                Url = url,
                PublishedAt = now.AddHours(-2),
            },
            LocalPath = path,
            FileHash = hash,
            ExtractedTextHash = ValueNormalizer.Sha256(string.Join('|', fields.Select(static field => field.Evidence))),
            ExtractionStatus = ExtractionStatus.Extracted,
            ParserVersion = "ui-smoke-v1",
            ParsedFields = fields,
            DownloadedAt = now,
        }, cancellationToken).ConfigureAwait(false);
    }

    private static IpoEvent Event(
        string id,
        Exchange exchange,
        Board board,
        string securityCode,
        string? applyCode,
        string name,
        DateOnly applyDate,
        decimal? issuePrice,
        int? lotSize,
        int? maximum,
        DataQualityStatus quality,
        IReadOnlyList<SubscriptionSession> sessions,
        DateTimeOffset now,
        string announcementUrl,
        IssueStatus status = IssueStatus.Active,
        IpoLifecycleStatus lifecycleStatus = IpoLifecycleStatus.ActiveUnconfirmed) => new()
        {
            Id = id,
            Exchange = exchange,
            Board = board,
            SecurityCode = securityCode,
            ApplyCode = applyCode,
            Name = name,
            ApplyDate = applyDate,
            IssuePrice = issuePrice,
            LotSize = lotSize,
            MaxApplyQuantity = maximum,
            Status = status,
            LifecycleStatus = lifecycleStatus,
            DataQualityStatus = quality,
            AnnouncementUrl = announcementUrl,
            FirstSeenAt = now.AddHours(-1),
            UpdatedAt = now,
            Sessions = sessions,
        };

    private static IReadOnlyList<SubscriptionSession> Sessions(Exchange exchange, TimeOnly cutoff)
    {
        var morningStart = exchange == Exchange.Shanghai ? new TimeOnly(9, 30) : new TimeOnly(9, 15);
        var fundingMode = exchange == Exchange.Beijing ? FundingMode.FullCash : FundingMode.MarketValue;
        return
        [
            new SubscriptionSession
            {
                SessionNumber = 1,
                OfficialStart = morningStart,
                OfficialEnd = new TimeOnly(11, 30),
                BrokerAcceptStart = morningStart,
                FundingMode = fundingMode,
                AllocationTimeSensitive = exchange == Exchange.Beijing,
                Source = "ui-smoke-announcement",
            },
            new SubscriptionSession
            {
                SessionNumber = 2,
                OfficialStart = new TimeOnly(13, 0),
                OfficialEnd = new TimeOnly(15, 0),
                SafetyCutoff = cutoff,
                FundingMode = fundingMode,
                AllocationTimeSensitive = exchange == Exchange.Beijing,
                Source = "ui-smoke-announcement",
            },
        ];
    }

    private static IReadOnlyList<SourceFieldValue> FieldsFor(
        IpoEvent ipoEvent,
        string source,
        int priority,
        DateTimeOffset now)
    {
        var fields = new List<SourceFieldValue>
        {
            Field("SecurityCode", ipoEvent.SecurityCode, source, priority, now),
            Field("Name", ipoEvent.Name, source, priority, now),
            Field("ApplyDate", ipoEvent.ApplyDate?.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture), source, priority, now),
            Field("IssueStatus", ipoEvent.Status.ToString(), source, priority, now),
            Field("OfficialSessions", string.Join(',', ipoEvent.Sessions.Select(static session => $"{session.OfficialStart:HH\\:mm}-{session.OfficialEnd:HH\\:mm}")), source, priority, now),
        };
        if (ipoEvent.ApplyCode is not null)
        {
            fields.Add(Field("ApplyCode", ipoEvent.ApplyCode, source, priority, now));
        }

        if (ipoEvent.IssuePrice is not null)
        {
            fields.Add(Field("IssuePrice", ipoEvent.IssuePrice.Value.ToString(CultureInfo.InvariantCulture), source, priority, now));
        }

        if (ipoEvent.LotSize is not null)
        {
            fields.Add(Field("LotSize", ipoEvent.LotSize.Value.ToString(CultureInfo.InvariantCulture), source, priority, now));
        }

        if (ipoEvent.MaxApplyQuantity is not null)
        {
            fields.Add(Field("MaxApplyQuantity", ipoEvent.MaxApplyQuantity.Value.ToString(CultureInfo.InvariantCulture), source, priority, now));
        }

        return fields;
    }

    private static SourceFieldValue Field(string name, string? value, string source, int priority, DateTimeOffset now) => new()
    {
        FieldName = name,
        RawValue = value,
        NormalizedValue = value,
        Source = source,
        Priority = priority,
        SourcePublishedAt = now.AddHours(-2),
        FetchedAt = now,
        RawHash = ValueNormalizer.Sha256($"{source}:{name}:{value}"),
    };

    private static ParsedAnnouncementField Parsed(string name, string value, string evidence) => new()
    {
        Name = name,
        Value = value,
        Confidence = 0.99m,
        Evidence = evidence,
        CharacterOffset = 0,
    };
}
