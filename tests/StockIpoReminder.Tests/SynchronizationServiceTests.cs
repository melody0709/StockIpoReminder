using Microsoft.Extensions.Logging.Abstractions;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class SynchronizationServiceTests
{
    [TestMethod]
    public async Task All_Failed_Sources_Preserve_Cached_Event_And_Reminders()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 24);
        var seeded = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:600001",
            date,
            lifecycle: IpoLifecycleStatus.ActiveUnconfirmed,
            status: IssueStatus.Active));
        var planner = new ReminderPlanner();
        await context.Repository.ReconcileReminderScheduleAsync(
            seeded.Event.Id,
            seeded.Event.EventVersion,
            planner.Plan(seeded.Event, new AppSettings()),
            context.TimeProvider.GetUtcNow());
        var before = await ScalarAsync<long>(context, "SELECT COUNT(*) FROM reminder_outbox WHERE delivery_state = 0;");

        var service = CreateService(
            context,
            planner,
            [new ResultCollector(Failed("failed-source", context.TimeProvider.GetUtcNow()))]);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);

        Assert.IsFalse(summary.Success);
        Assert.IsNotNull(await context.Repository.GetEventAsync(seeded.Event.Id));
        Assert.AreEqual(before, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM reminder_outbox WHERE delivery_state = 0;"));
    }

    [TestMethod]
    public async Task Throwing_Source_Does_Not_Block_A_Successful_Source()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var candidate = Candidate("healthy-source", Exchange.Shanghai, "600002", new DateOnly(2026, 8, 25));
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [
                new ThrowingCollector("throwing-source"),
                new ResultCollector(Succeeded("healthy-source", context.TimeProvider.GetUtcNow(), candidate)),
            ]);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);

        Assert.IsTrue(summary.Success);
        Assert.AreEqual(1, summary.SuccessfulSources);
        Assert.AreEqual(1, summary.FailedSources);
        Assert.IsNotNull(await context.Repository.GetEventAsync("shanghai:600002"));
        Assert.AreEqual(1L, await ScalarAsync<long>(context, "SELECT consecutive_failures FROM source_health WHERE source = 'throwing-source';"));
    }

    [TestMethod]
    public async Task Disabled_Market_Does_Not_Create_An_Event_Or_Reminder_Plan()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        await context.Repository.SaveSettingsAsync(new AppSettings { BeijingEnabled = false });
        var candidate = Candidate("bse", Exchange.Beijing, "920001", new DateOnly(2026, 8, 25));
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("bse", context.TimeProvider.GetUtcNow(), candidate))]);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);

        Assert.IsTrue(summary.Success);
        Assert.AreEqual(0, summary.CandidateCount);
        Assert.AreEqual(0, summary.EventCount);
        Assert.IsNull(await context.Repository.GetEventAsync("beijing:920001"));
        Assert.AreEqual(0L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM reminder_outbox;"));
    }

    [TestMethod]
    public async Task Critical_Change_After_Acknowledgement_Requires_Reconfirmation_And_Replans()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 24);
        var seeded = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:600003",
            date,
            issuePrice: 10.50m,
            lifecycle: IpoLifecycleStatus.ActiveUnconfirmed,
            status: IssueStatus.Active));
        await context.Repository.AcknowledgeAsync(
            seeded.Event.Id,
            seeded.Event.EventVersion,
            context.TimeProvider.GetUtcNow(),
            EventDataHasher.Compute(seeded.Event));
        var candidate = Candidate("eastmoney", Exchange.Shanghai, "600003", date, issuePrice: 11.25m, status: IssueStatus.Active);
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("eastmoney", context.TimeProvider.GetUtcNow(), candidate))]);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);
        var updated = await context.Repository.GetEventAsync(seeded.Event.Id);

        Assert.IsTrue(summary.Success);
        Assert.IsNotNull(updated);
        Assert.AreEqual(2, updated.EventVersion);
        Assert.AreEqual(11.25m, updated.IssuePrice);
        Assert.AreEqual(IpoLifecycleStatus.AcknowledgedNeedsReview, updated.LifecycleStatus);
        Assert.IsGreaterThan(0L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id = 'shanghai:600003' AND event_version = 2 AND delivery_state = 0;"));
    }

    [TestMethod]
    public async Task Missing_Public_Field_During_Sync_Does_Not_Promote_Manual_Override_Into_Public_Data()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 26);
        var seeded = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:600004",
            date,
            issuePrice: 10.50m,
            sessions: MarketSessionFactory.CreateDefault(Exchange.Shanghai, new AppSettings())));
        await context.Repository.AddManualOverrideAsync(
            seeded.Event.Id,
            seeded.Event.EventVersion,
            nameof(IpoEvent.IssuePrice),
            "20.50",
            "正式公告人工核验",
            announcementDocumentId: null);
        var candidate = Candidate("official-list", Exchange.Shanghai, "600004", date, issuePrice: null) with
        {
            ApplyCode = "730001",
            MaxApplyQuantity = 15_000,
        };
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("official-list", context.TimeProvider.GetUtcNow(), candidate))]);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);
        var effective = await context.Repository.GetEventAsync(seeded.Event.Id);

        Assert.IsTrue(summary.Success);
        Assert.IsNotNull(effective);
        Assert.AreEqual(1, effective.EventVersion);
        Assert.AreEqual(20.50m, effective.IssuePrice);
        Assert.AreEqual(10.50m, await ScalarAsync<decimal>(context, "SELECT issue_price FROM ipo_events WHERE id = 'shanghai:600004';"));
        Assert.AreEqual(1L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM manual_overrides WHERE ipo_event_id = 'shanghai:600004' AND event_version = 1 AND revoked_at IS NULL;"));
    }

    [TestMethod]
    public async Task Known_Announcement_Id_Is_Redownloaded_So_Changed_Content_Creates_A_New_Version()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 25);
        var candidate = Candidate("eastmoney", Exchange.Shanghai, "600005", date, issuePrice: 9.90m);
        var reference = AnnouncementReference("same-announcement");
        var processor = new SequenceAnnouncementProcessor(
            Document(reference, "hash-v1", "10.10"),
            Document(reference, "hash-v2", "11.20"));
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("eastmoney", context.TimeProvider.GetUtcNow(), candidate))],
            [new StaticAnnouncementProvider(reference)],
            processor);

        Assert.IsTrue((await service.SynchronizeAsync("first", CancellationToken.None)).Success);
        Assert.IsTrue((await service.SynchronizeAsync("second", CancellationToken.None)).Success);

        var updated = await context.Repository.GetEventAsync("shanghai:600005");
        Assert.IsNotNull(updated);
        Assert.AreEqual(2, updated.EventVersion);
        Assert.AreEqual(11.20m, updated.IssuePrice);
        Assert.AreEqual(2, (await context.Repository.GetAnnouncementsAsync(updated.Id)).Count);
        Assert.AreEqual(2, processor.CallCount);
    }

    [TestMethod]
    public async Task Failed_Announcement_Extraction_Marks_Single_Source_Event_For_Manual_Review()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 25);
        var candidate = Candidate("eastmoney", Exchange.Shanghai, "600006", date);
        var reference = AnnouncementReference("failed-announcement");
        var failed = Document(reference, "failed-hash", issuePrice: null) with
        {
            ExtractionStatus = ExtractionStatus.Failed,
            ParsedFields = [],
        };
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("eastmoney", context.TimeProvider.GetUtcNow(), candidate))],
            [new StaticAnnouncementProvider(reference)],
            new SequenceAnnouncementProcessor(failed));

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);
        var ipoEvent = await context.Repository.GetEventAsync("shanghai:600006");

        Assert.IsTrue(summary.Success);
        Assert.IsNotNull(ipoEvent);
        Assert.AreEqual(DataQualityStatus.ManualReviewRequired, ipoEvent.DataQualityStatus);
        Assert.AreEqual(1, (await context.Repository.GetAnnouncementsAsync(ipoEvent.Id)).Count);
    }

    [TestMethod]
    public async Task Thrown_Announcement_Download_Marks_NearTerm_Event_And_Source_For_Review()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 25);
        var candidate = Candidate("eastmoney", Exchange.Shanghai, "600008", date);
        var reference = AnnouncementReference("pseudo-pdf");
        var backoffStore = new SourceBackoffStore(
            context.Options,
            new SourceBackoffOptions { JitterRatio = 0 });
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("eastmoney", context.TimeProvider.GetUtcNow(), candidate))],
            [new StaticAnnouncementProvider(reference)],
            new ThrowingAnnouncementProcessor(new InvalidDataException("缺少 %PDF 文件签名")),
            backoffStore);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);
        var ipoEvent = await context.Repository.GetEventAsync("shanghai:600008");
        var health = await context.Repository.GetHealthSummaryAsync(
            date,
            context.TimeProvider.GetUtcNow(),
            CancellationToken.None);
        var announcementHealth = health.Sources.Single(static source => source.Source == "official-announcement");

        Assert.IsTrue(summary.Success);
        Assert.IsNotNull(ipoEvent);
        Assert.AreEqual(DataQualityStatus.ManualReviewRequired, ipoEvent.DataQualityStatus);
        Assert.AreEqual(HealthState.Failed, announcementHealth.State);
        StringAssert.Contains(announcementHealth.LastError!, "1/1 个文档下载或解析失败");
        Assert.AreEqual(0, (await context.Repository.GetAnnouncementsAsync(ipoEvent.Id)).Count);
    }

    [TestMethod]
    public async Task Explicit_Empty_Announcement_Result_Marks_NearTerm_Event_For_Manual_Review()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var date = new DateOnly(2026, 8, 25);
        var candidate = Candidate("eastmoney", Exchange.Shanghai, "600009", date);
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [new ResultCollector(Succeeded("eastmoney", context.TimeProvider.GetUtcNow(), candidate))],
            [new EmptyAnnouncementProvider("official-announcement")]);

        var summary = await service.SynchronizeAsync("test", CancellationToken.None);
        var ipoEvent = await context.Repository.GetEventAsync("shanghai:600009");

        Assert.IsTrue(summary.Success);
        Assert.IsNotNull(ipoEvent);
        Assert.AreEqual(DataQualityStatus.ManualReviewRequired, ipoEvent.DataQualityStatus);
    }

    [TestMethod]
    public async Task RetryAfter_Backoff_On_One_Source_Does_Not_Block_A_Healthy_Source()
    {
        await using var context = await RepositoryTestContext.CreateAsync();
        var now = context.TimeProvider.GetUtcNow();
        var delayed = new CountingCollector(new CollectorResult
        {
            Source = "rate-limited-source",
            Success = false,
            StartedAt = now,
            FinishedAt = now,
            Error = "HTTP 429",
            RetryAfter = TimeSpan.FromMinutes(10),
        });
        var candidate = Candidate("healthy-source", Exchange.Shanghai, "600007", new DateOnly(2026, 8, 25));
        var healthy = new CountingCollector(Succeeded("healthy-source", now, candidate));
        var backoffStore = new SourceBackoffStore(
            context.Options,
            new SourceBackoffOptions { JitterRatio = 0 });
        var service = CreateService(
            context,
            new ReminderPlanner(),
            [delayed, healthy],
            sourceBackoffStore: backoffStore);

        var first = await service.SynchronizeAsync("first", CancellationToken.None);
        var decision = await backoffStore.GetDecisionAsync("rate-limited-source", now);
        var second = await service.SynchronizeAsync("second", CancellationToken.None);

        Assert.IsTrue(first.Success);
        Assert.AreEqual(1, first.SuccessfulSources);
        Assert.AreEqual(1, first.FailedSources);
        Assert.AreEqual(0, first.DeferredSources);
        Assert.IsFalse(decision.CanAttempt);
        Assert.AreEqual(now.AddMinutes(10), decision.NextAttemptAt);
        Assert.IsTrue(second.Success);
        Assert.AreEqual(1, second.SuccessfulSources);
        Assert.AreEqual(0, second.FailedSources);
        Assert.AreEqual(1, second.DeferredSources);
        Assert.AreEqual(1, delayed.CallCount);
        Assert.AreEqual(2, healthy.CallCount);
        Assert.IsNotNull(await context.Repository.GetEventAsync("shanghai:600007"));
        Assert.AreEqual(1L, await ScalarAsync<long>(context, "SELECT COUNT(*) FROM sync_runs WHERE source = 'rate-limited-source';"));
    }

    private static SynchronizationService CreateService(
        RepositoryTestContext context,
        ReminderPlanner planner,
        IReadOnlyList<IIpoCollector> collectors,
        IReadOnlyList<IAnnouncementProvider>? announcementProviders = null,
        IAnnouncementProcessor? announcementProcessor = null,
        SourceBackoffStore? sourceBackoffStore = null) =>
        new(
            collectors,
            announcementProviders ?? [],
            announcementProcessor ?? new UnusedAnnouncementProcessor(),
            context.Repository,
            new IpoReconciler(),
            planner,
            new RuntimeState(),
            context.TimeProvider,
            NullLogger<SynchronizationService>.Instance,
            sourceBackoffStore);

    private static CollectorResult Succeeded(string source, DateTimeOffset now, params IpoCandidate[] candidates) => new()
    {
        Source = source,
        Success = true,
        StartedAt = now,
        FinishedAt = now,
        Candidates = candidates,
        RecordCount = candidates.Length,
        RawPayload = "{}",
        RawHash = $"{source}-hash",
        SchemaFingerprint = $"{source}-schema",
    };

    private static CollectorResult Failed(string source, DateTimeOffset now) => new()
    {
        Source = source,
        Success = false,
        StartedAt = now,
        FinishedAt = now,
        Error = "fixture failure",
    };

    private static IpoCandidate Candidate(
        string source,
        Exchange exchange,
        string securityCode,
        DateOnly applyDate,
        decimal? issuePrice = 10.50m,
        IssueStatus status = IssueStatus.Upcoming) => new()
    {
        Source = source,
        SourcePriority = 500,
        FetchedAt = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero),
        Exchange = exchange,
        Board = exchange == Exchange.Beijing ? Board.Beijing : Board.Main,
        SecurityCode = securityCode,
        ApplyCode = exchange == Exchange.Beijing ? securityCode : $"7{securityCode[1..]}",
        Name = $"测试股份{securityCode}",
        ApplyDate = applyDate,
        IssuePrice = issuePrice,
        LotSize = 500,
        MaxApplyQuantity = 10_000,
        Status = status,
    };

    private static AnnouncementReference AnnouncementReference(string id) => new()
    {
        Provider = "official-announcement",
        AnnouncementId = id,
        Title = "首次公开发行股票发行公告",
        Url = new Uri($"https://www.sse.com.cn/test/{id}.pdf"),
        PublishedAt = new DateTimeOffset(2026, 8, 24, 1, 0, 0, TimeSpan.Zero),
        AnnouncementType = "发行公告",
    };

    private static AnnouncementDocument Document(AnnouncementReference reference, string hash, string? issuePrice) => new()
    {
        Id = $"{reference.Provider}:{reference.AnnouncementId}:{hash}",
        IpoEventId = "placeholder",
        Reference = reference,
        LocalPath = $"C:\\fixture\\{hash}.pdf",
        FileHash = hash,
        ExtractedTextHash = hash,
        ExtractionStatus = ExtractionStatus.Extracted,
        ParserVersion = "fixture",
        ParsedFields = issuePrice is null
            ? []
            :
            [
                new ParsedAnnouncementField
                {
                    Name = "IssuePrice",
                    Value = issuePrice,
                    Confidence = 0.99m,
                    Evidence = $"发行价格为{issuePrice}元",
                    CharacterOffset = 0,
                },
            ],
        DownloadedAt = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero),
    };

    private static async Task<T> ScalarAsync<T>(RepositoryTestContext context, string sql)
    {
        await using var connection = await context.OpenConnectionAsync();
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        var value = await command.ExecuteScalarAsync();
        return (T)Convert.ChangeType(value!, typeof(T), System.Globalization.CultureInfo.InvariantCulture);
    }

    private sealed class ResultCollector(CollectorResult result) : IIpoCollector
    {
        public string SourceName => result.Source;
        public int Priority => 500;
        public Task<CollectorResult> CollectAsync(CancellationToken cancellationToken) => Task.FromResult(result);
    }

    private sealed class CountingCollector(CollectorResult result) : IIpoCollector
    {
        public string SourceName => result.Source;
        public int Priority => 500;
        public int CallCount { get; private set; }

        public Task<CollectorResult> CollectAsync(CancellationToken cancellationToken)
        {
            CallCount++;
            return Task.FromResult(result);
        }
    }

    private sealed class ThrowingCollector(string sourceName) : IIpoCollector
    {
        public string SourceName => sourceName;
        public int Priority => 500;
        public Task<CollectorResult> CollectAsync(CancellationToken cancellationToken) => throw new HttpRequestException("fixture exception");
    }

    private sealed class StaticAnnouncementProvider(AnnouncementReference reference) : IAnnouncementProvider
    {
        public string ProviderName => reference.Provider;
        public bool Supports(Exchange exchange) => exchange == Exchange.Shanghai;
        public Task<IReadOnlyList<AnnouncementReference>> SearchAsync(
            IpoEvent ipoEvent,
            DateOnly from,
            DateOnly to,
            CancellationToken cancellationToken) => Task.FromResult<IReadOnlyList<AnnouncementReference>>([reference]);
    }

    private sealed class EmptyAnnouncementProvider(string providerName) : IAnnouncementProvider
    {
        public string ProviderName => providerName;
        public bool Supports(Exchange exchange) => exchange == Exchange.Shanghai;

        public Task<IReadOnlyList<AnnouncementReference>> SearchAsync(
            IpoEvent ipoEvent,
            DateOnly from,
            DateOnly to,
            CancellationToken cancellationToken) => Task.FromResult<IReadOnlyList<AnnouncementReference>>([]);
    }

    private sealed class SequenceAnnouncementProcessor(params AnnouncementDocument[] documents) : IAnnouncementProcessor
    {
        private int _index;
        public int CallCount => _index;

        public Task<AnnouncementDocument> DownloadAndParseAsync(
            IpoEvent ipoEvent,
            AnnouncementReference announcement,
            CancellationToken cancellationToken)
        {
            if (_index >= documents.Length)
            {
                throw new InvalidOperationException("No announcement fixture remains.");
            }

            return Task.FromResult(documents[_index++] with { IpoEventId = ipoEvent.Id });
        }
    }

    private sealed class ThrowingAnnouncementProcessor(Exception exception) : IAnnouncementProcessor
    {
        public Task<AnnouncementDocument> DownloadAndParseAsync(
            IpoEvent ipoEvent,
            AnnouncementReference announcement,
            CancellationToken cancellationToken) => Task.FromException<AnnouncementDocument>(exception);
    }

    private sealed class UnusedAnnouncementProcessor : IAnnouncementProcessor
    {
        public Task<AnnouncementDocument> DownloadAndParseAsync(
            IpoEvent ipoEvent,
            AnnouncementReference announcement,
            CancellationToken cancellationToken) => throw new InvalidOperationException("No announcement providers were configured for this test.");
    }
}
