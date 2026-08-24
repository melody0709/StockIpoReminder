using System.Globalization;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.Data.Sqlite;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure;
using StockIpoReminder.Infrastructure.Announcements;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Persistence;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Diagnostics;

public sealed class DiagnosticRunner
{
    private static readonly string[] ExpectedCollectors = ["eastmoney", "sse", "cninfo", "bse"];
    private const string ExpectedBseUrl = "https://www.bseinfo.net/disclosure/2026/2026-08-20/1787216709992_052262.pdf";
    private const string ExpectedBseSha256 = "6d9f2fa667d0a95676dd87a8fde550c420fb37783f6dd5b01117277e0e037b26";
    private const int ExpectedBseFileLength = 467078;
    private readonly DiagnosticOptions _options;

    public DiagnosticRunner(DiagnosticOptions options)
    {
        _options = options;
    }

    public async Task<DiagnosticReport> RunAsync()
    {
        var workspace = TemporaryWorkspace.Create();
        var startedAt = ChinaTime.Now(TimeProvider.System);
        var report = new DiagnosticReport
        {
            RunId = $"{startedAt:yyyyMMddTHHmmss}-{Guid.NewGuid():N}",
            Mode = _options.Mode,
            StartedAt = startedAt,
            Environment = new DiagnosticEnvironment
            {
                OsDescription = RuntimeInformation.OSDescription,
                FrameworkDescription = RuntimeInformation.FrameworkDescription,
                ProcessArchitecture = RuntimeInformation.ProcessArchitecture.ToString(),
                ShanghaiDate = ChinaTime.Today(TimeProvider.System).ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            },
            DataIsolation = new DataIsolationDiagnostic
            {
                TemporaryDirectoryId = workspace.DirectoryId,
                UsedLocalApplicationData = false,
                KeepRequested = _options.KeepTemporaryData,
            },
        };

        AddCheck(
            report,
            "isolation.temporary-root",
            workspace.IsSafelyIsolated,
            "诊断数据目录位于系统临时目录，并且不在正式 LocalAppData 数据目录内。");

        ServiceProvider? serviceProvider = null;
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(_options.TimeoutSeconds));
        ConsoleCancelEventHandler cancelHandler = (_, eventArgs) =>
        {
            eventArgs.Cancel = true;
            timeout.Cancel();
        };
        Console.CancelKeyPress += cancelHandler;

        try
        {
            var services = new ServiceCollection();
            services.AddLogging(logging =>
            {
                logging.ClearProviders();
                logging.SetMinimumLevel(LogLevel.Critical);
            });
            services.AddStockIpoReminderInfrastructure(workspace.DataRoot);
            services.AddSingleton<IReminderSink, NullReminderSink>();
            serviceProvider = services.BuildServiceProvider(new ServiceProviderOptions
            {
                ValidateScopes = true,
            });

            var repository = serviceProvider.GetRequiredService<IIpoRepository>();
            await repository.InitializeAsync(timeout.Token).ConfigureAwait(false);
            AddCheck(report, "isolation.database-initialized", File.Exists(workspace.DatabasePath), "临时 SQLite 已初始化。");

            if (_options.RunSync)
            {
                report.Synchronization = await RunSynchronizationAsync(
                    serviceProvider,
                    repository,
                    workspace,
                    report,
                    timeout.Token).ConfigureAwait(false);
            }

            if (_options.RunBseSample)
            {
                report.BseSample = await RunBseSampleAsync(
                    serviceProvider,
                    workspace,
                    report,
                    timeout.Token).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException) when (timeout.IsCancellationRequested)
        {
            report.FatalError = $"诊断在 {_options.TimeoutSeconds} 秒超时或被用户取消。";
            AddCheck(report, "run.completed-within-timeout", false, report.FatalError);
        }
        catch (Exception ex)
        {
            report.FatalError = Safe(ex.ToString(), workspace.DataRoot, 4000);
            AddCheck(report, "run.unhandled-error", false, ex.Message);
        }
        finally
        {
            Console.CancelKeyPress -= cancelHandler;
            if (serviceProvider is not null)
            {
                await serviceProvider.DisposeAsync().ConfigureAwait(false);
            }

            SqliteConnection.ClearAllPools();
            if (_options.KeepTemporaryData)
            {
                report.DataIsolation.RetainedDataRoot = workspace.DataRoot;
                report.DataIsolation.CleanupSucceeded = true;
                AddCheck(report, "isolation.retained-by-request", true, "--keep 已显式请求保留临时诊断目录。");
            }
            else
            {
                report.DataIsolation.CleanupAttempted = true;
                var cleanup = await workspace.DeleteAsync().ConfigureAwait(false);
                report.DataIsolation.CleanupSucceeded = cleanup.Success;
                report.DataIsolation.CleanupError = cleanup.Error;
                AddCheck(
                    report,
                    "isolation.temporary-data-cleaned",
                    cleanup.Success,
                    cleanup.Success ? "临时数据库、公告和日志目录已删除。" : cleanup.Error ?? "临时目录删除失败。");
            }
        }

        report.FinishedAt = ChinaTime.Now(TimeProvider.System);
        report.FatalError = Safe(report.FatalError, workspace.DataRoot, 4000);
        report.Success = report.FatalError is null
            && report.Checks.Count > 0
            && report.Checks.All(static check => check.Passed);
        return report;
    }

    private static async Task<SyncDiagnostic> RunSynchronizationAsync(
        IServiceProvider services,
        IIpoRepository repository,
        TemporaryWorkspace workspace,
        DiagnosticReport report,
        CancellationToken cancellationToken)
    {
        var runStartedAt = ChinaTime.Now(TimeProvider.System);
        var summary = await services.GetRequiredService<SynchronizationService>()
            .SynchronizeAsync("isolated-diagnostic", cancellationToken)
            .ConfigureAwait(false);
        var now = ChinaTime.Now(TimeProvider.System);
        var today = ChinaTime.Today(TimeProvider.System);
        var health = await repository.GetHealthSummaryAsync(today, now, cancellationToken).ConfigureAwait(false);
        var sourceDiagnostics = health.Sources
            .Select(source => new SourceDiagnostic
            {
                Source = source.Source,
                AttemptedThisRun = source.LastAttemptAt >= runStartedAt,
                SucceededThisRun = source.LastSuccessAt >= runStartedAt && source.ConsecutiveFailures == 0,
                LastAttemptAt = source.LastAttemptAt,
                LastSuccessAt = source.LastSuccessAt,
                RecordCount = source.LastRecordCount,
                ConsecutiveFailures = source.ConsecutiveFailures,
                State = source.State,
                SchemaFingerprint = source.SchemaFingerprint,
                Error = Safe(source.LastError, workspace.DataRoot, 1500),
            })
            .OrderBy(static source => source.Source, StringComparer.Ordinal)
            .ToArray();

        var expectedSources = ExpectedCollectors
            .Select(expected => sourceDiagnostics.FirstOrDefault(source => string.Equals(source.Source, expected, StringComparison.OrdinalIgnoreCase)))
            .ToArray();
        var attempted = expectedSources.All(static source => source?.AttemptedThisRun == true);
        var succeeded = expectedSources.All(static source => source?.SucceededThisRun == true);
        AddCheck(report, "sync.service-succeeded", summary.Success, summary.Error ?? "SynchronizationService 返回成功。");
        AddCheck(report, "sync.four-collectors-attempted", attempted, DescribeMissing(expectedSources, static source => source?.AttemptedThisRun == true));
        AddCheck(report, "sync.four-collectors-succeeded", succeeded, DescribeMissing(expectedSources, static source => source?.SucceededThisRun == true));

        var integrity = await CheckDatabaseIntegrityAsync(workspace.DatabasePath, cancellationToken).ConfigureAwait(false);
        AddCheck(report, "sync.database-integrity", string.Equals(integrity, "ok", StringComparison.OrdinalIgnoreCase), $"PRAGMA integrity_check: {integrity}");

        var events = await repository.GetEventsAsync(today.AddDays(-30), today.AddDays(120), cancellationToken).ConfigureAwait(false);
        var orderedEvents = events
            .OrderBy(static item => item.ApplyDate)
            .ThenBy(static item => item.Exchange)
            .ThenBy(static item => item.SecurityCode, StringComparer.Ordinal)
            .ToArray();
        const int eventLimit = 150;
        var eventDiagnostics = new List<EventDiagnostic>(Math.Min(events.Count, eventLimit));
        var announcementsByEvent = new Dictionary<string, IReadOnlyList<AnnouncementDocument>>(StringComparer.OrdinalIgnoreCase);
        foreach (var ipoEvent in orderedEvents)
        {
            var announcements = await repository.GetAnnouncementsAsync(ipoEvent.Id, cancellationToken).ConfigureAwait(false);
            announcementsByEvent[ipoEvent.Id] = announcements;
            if (eventDiagnostics.Count < eventLimit)
            {
                var fieldSources = await repository.GetFieldSourcesAsync(ipoEvent.Id, cancellationToken).ConfigureAwait(false);
                eventDiagnostics.Add(ToEventDiagnostic(ipoEvent, fieldSources, announcements, workspace));
            }
        }

        var exchangeCounts = events
            .GroupBy(static item => item.Exchange.ToString())
            .OrderBy(static group => group.Key, StringComparer.Ordinal)
            .ToDictionary(static group => group.Key, static group => group.Count(), StringComparer.Ordinal);

        var allExchangesPresent = new[] { Exchange.Shanghai, Exchange.Shenzhen, Exchange.Beijing }
            .All(exchange => events.Any(item => item.Exchange == exchange));
        AddCheck(
            report,
            "sync.shanghai-shenzhen-beijing-present",
            allExchangesPresent,
            $"Shanghai={exchangeCounts.GetValueOrDefault(nameof(Exchange.Shanghai))}; Shenzhen={exchangeCounts.GetValueOrDefault(nameof(Exchange.Shenzhen))}; Beijing={exchangeCounts.GetValueOrDefault(nameof(Exchange.Beijing))}");

        var announcementScopeEvents = orderedEvents
            .Where(item => IsAnnouncementScopeEvent(item, today))
            .ToArray();
        var expectedAnnouncementProviders = announcementScopeEvents
            .Select(static item => AnnouncementProviderFor(item.Exchange))
            .Where(static provider => provider is not null)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .Cast<string>()
            .OrderBy(static provider => provider, StringComparer.Ordinal)
            .ToArray();
        var expectedAnnouncementSources = expectedAnnouncementProviders
            .Select(expected => sourceDiagnostics.FirstOrDefault(source =>
                string.Equals(source.Source, expected, StringComparison.OrdinalIgnoreCase)))
            .ToArray();
        var allAnnouncementProvidersAttempted = expectedAnnouncementSources
            .All(static source => source?.AttemptedThisRun == true);
        AddCheck(
            report,
            "sync.expected-announcement-providers-attempted",
            allAnnouncementProvidersAttempted,
            DescribeNamedMissing(
                expectedAnnouncementProviders,
                expectedAnnouncementSources,
                static source => source?.AttemptedThisRun == true));

        var usableAnnouncementEventCount = announcementScopeEvents.Count(item =>
            announcementsByEvent.GetValueOrDefault(item.Id)?.Any(HasUsableAnnouncementEvidence) == true);
        var controlledManualReviewCount = announcementScopeEvents.Count(item =>
            announcementsByEvent.GetValueOrDefault(item.Id)?.Any(HasUsableAnnouncementEvidence) != true
            && item.DataQualityStatus == DataQualityStatus.ManualReviewRequired);
        var uncoveredAnnouncementEvents = announcementScopeEvents
            .Where(item => announcementsByEvent.GetValueOrDefault(item.Id)?.Any(HasUsableAnnouncementEvidence) != true
                && item.DataQualityStatus != DataQualityStatus.ManualReviewRequired)
            .ToArray();
        AddCheck(
            report,
            "sync.announcement-scope-covered",
            uncoveredAnnouncementEvents.Length == 0,
            uncoveredAnnouncementEvents.Length == 0
                ? $"范围内 {announcementScopeEvents.Length} 个事件均有可用正式公告，或已明确进入 manual_review_required。"
                : $"未覆盖事件：{string.Join(',', uncoveredAnnouncementEvents.Select(static item => item.Id))}");

        var failedAnnouncementSources = expectedAnnouncementProviders
            .Select(provider => new
            {
                Provider = provider,
                Source = sourceDiagnostics.FirstOrDefault(source =>
                    string.Equals(source.Source, provider, StringComparison.OrdinalIgnoreCase)),
            })
            .Where(static item => item.Source?.SucceededThisRun != true)
            .ToArray();
        var providersSucceededOrControlled = failedAnnouncementSources.All(item =>
        {
            var exchange = ExchangeForAnnouncementProvider(item.Provider);
            return exchange is not null
                && announcementScopeEvents
                    .Where(ipoEvent => ipoEvent.Exchange == exchange)
                    .All(ipoEvent => announcementsByEvent.GetValueOrDefault(ipoEvent.Id)?.Any(HasUsableAnnouncementEvidence) == true
                        || ipoEvent.DataQualityStatus == DataQualityStatus.ManualReviewRequired);
        });
        AddCheck(
            report,
            "sync.announcement-providers-succeeded-or-controlled",
            providersSucceededOrControlled,
            failedAnnouncementSources.Length == 0
                ? "本轮预期公告 Provider 均成功。"
                : $"失败或未成功 Provider：{string.Join(',', failedAnnouncementSources.Select(static item => item.Provider))}；对应事件必须全部有缓存公告或 manual_review_required。" );

        var allAnnouncements = announcementsByEvent.Values.SelectMany(static documents => documents).ToArray();
        var invalidPdfDocuments = new List<AnnouncementDocument>();
        foreach (var document in allAnnouncements.Where(static document =>
            document.Reference.Url.AbsolutePath.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase)
            || document.LocalPath.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase)))
        {
            if (!await HasValidPdfSignatureAsync(document.LocalPath, workspace, cancellationToken).ConfigureAwait(false))
            {
                invalidPdfDocuments.Add(document);
            }
        }

        AddCheck(
            report,
            "sync.downloaded-pdf-signatures-valid",
            invalidPdfDocuments.Count == 0,
            invalidPdfDocuments.Count == 0
                ? $"已保存的 {allAnnouncements.Count(static document => document.LocalPath.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase))} 个 PDF 均具有有效文件签名。"
                : $"无效 PDF：{string.Join(',', invalidPdfDocuments.Select(static document => $"{document.Reference.Provider}/{document.Reference.AnnouncementId}"))}" );

        var errorsAreRedacted = sourceDiagnostics.All(source => IsSafelyRedacted(source.Error, workspace.DataRoot));
        AddCheck(
            report,
            "sync.errors-are-redacted",
            errorsAreRedacted,
            errorsAreRedacted ? "来源错误不含临时目录、工作区绝对路径、敏感请求头或未脱敏 URL query。" : "至少一个来源错误仍包含敏感路径、凭据头或 URL query。" );

        return new SyncDiagnostic
        {
            ServiceSucceeded = summary.Success,
            SuccessfulSources = summary.SuccessfulSources,
            FailedSources = summary.FailedSources,
            DeferredSources = summary.DeferredSources,
            CandidateCount = summary.CandidateCount,
            EventCount = summary.EventCount,
            AnnouncementCount = summary.AnnouncementCount,
            Error = Safe(summary.Error, workspace.DataRoot, 1500),
            DatabaseIntegrity = integrity,
            PersistedEventCount = events.Count,
            EventsTruncated = events.Count > eventLimit,
            ExpectedAnnouncementProviderCount = expectedAnnouncementProviders.Length,
            AttemptedAnnouncementProviderCount = expectedAnnouncementSources.Count(static source => source?.AttemptedThisRun == true),
            FailedAnnouncementProviderCount = failedAnnouncementSources.Length,
            AnnouncementScopeEventCount = announcementScopeEvents.Length,
            EventsWithUsableAnnouncementCount = usableAnnouncementEventCount,
            EventsWithControlledManualReviewCount = controlledManualReviewCount,
            UncoveredAnnouncementEventCount = uncoveredAnnouncementEvents.Length,
            InvalidPdfCount = invalidPdfDocuments.Count,
            ExchangeCounts = exchangeCounts,
            Sources = sourceDiagnostics,
            Events = eventDiagnostics,
        };
    }

    private static async Task<BseSampleDiagnostic> RunBseSampleAsync(
        IServiceProvider services,
        TemporaryWorkspace workspace,
        DiagnosticReport report,
        CancellationToken cancellationToken)
    {
        var firstCheck = report.Checks.Count;
        try
        {
            var now = ChinaTime.Now(TimeProvider.System);
            var ipoEvent = new IpoEvent
            {
                Id = "beijing:920289",
                Exchange = Exchange.Beijing,
                Board = Board.Beijing,
                SecurityCode = "920289",
                ApplyCode = "920289",
                LegacyCode = "874378",
                Name = "华汇智能",
                ApplyDate = new DateOnly(2026, 8, 24),
                IssuePrice = 17.71m,
                Status = IssueStatus.Active,
                LifecycleStatus = IpoLifecycleStatus.ActiveUnconfirmed,
                AnnouncementUrl = "https://www.bseinfo.net/newshare/listofissues_detail.html?id=346",
                FirstSeenAt = now,
                UpdatedAt = now,
                Sessions =
                [
                    new SubscriptionSession
                    {
                        SessionNumber = 1,
                        OfficialStart = new TimeOnly(9, 15),
                        OfficialEnd = new TimeOnly(11, 30),
                        FundingMode = FundingMode.FullCash,
                        AllocationTimeSensitive = true,
                        Source = "bse-sample",
                    },
                    new SubscriptionSession
                    {
                        SessionNumber = 2,
                        OfficialStart = new TimeOnly(13, 0),
                        OfficialEnd = new TimeOnly(15, 0),
                        FundingMode = FundingMode.FullCash,
                        AllocationTimeSensitive = true,
                        Source = "bse-sample",
                    },
                ],
            };

            var provider = services.GetRequiredService<BseAnnouncementProvider>();
            var references = await provider.SearchAsync(
                ipoEvent,
                new DateOnly(2026, 8, 19),
                new DateOnly(2026, 8, 25),
                cancellationToken).ConfigureAwait(false);
            var selected = references.FirstOrDefault(reference =>
                    string.Equals(WithoutQuery(reference.Url), ExpectedBseUrl, StringComparison.OrdinalIgnoreCase))
                ?? references.FirstOrDefault(reference => reference.Title.Contains("发行公告", StringComparison.Ordinal));

            AddCheck(report, "bse-sample.reference-found", selected is not null, $"公告候选数量：{references.Count}。");
            if (selected is null)
            {
                return new BseSampleDiagnostic
                {
                    Success = false,
                    ReferenceCount = references.Count,
                    Error = "未找到华汇智能正式发行公告。",
                };
            }

            var selectedUrl = WithoutQuery(selected.Url);
            AddCheck(report, "bse-sample.expected-official-url", string.Equals(selectedUrl, ExpectedBseUrl, StringComparison.OrdinalIgnoreCase), selectedUrl);
            AddCheck(report, "bse-sample.url-has-no-query", string.IsNullOrEmpty(selected.Url.Query), "报告和下载目标均不包含查询参数。");

            var document = await services.GetRequiredService<IAnnouncementProcessor>()
                .DownloadAndParseAsync(ipoEvent, selected, cancellationToken)
                .ConfigureAwait(false);
            var fileBytes = await File.ReadAllBytesAsync(document.LocalPath, cancellationToken).ConfigureAwait(false);
            var calculatedHash = ValueNormalizer.Sha256(fileBytes);
            var fileWithinWorkspace = IsWithin(document.LocalPath, workspace.DataRoot);
            AddCheck(report, "bse-sample.file-stored-in-temporary-root", fileWithinWorkspace, "公告只写入隔离临时目录。");
            AddCheck(report, "bse-sample.file-hash-matches", string.Equals(calculatedHash, document.FileHash, StringComparison.OrdinalIgnoreCase), calculatedHash);
            AddCheck(report, "bse-sample.expected-file-hash", string.Equals(calculatedHash, ExpectedBseSha256, StringComparison.OrdinalIgnoreCase), calculatedHash);
            AddCheck(
                report,
                "bse-sample.expected-pdf-length",
                fileBytes.Length == ExpectedBseFileLength && fileBytes.AsSpan().StartsWith("%PDF"u8),
                $"expected={ExpectedBseFileLength}; actual={fileBytes.Length}");
            AddCheck(report, "bse-sample.text-extracted", document.ExtractionStatus == ExtractionStatus.Extracted, document.ExtractionStatus.ToString());

            var fields = document.ParsedFields
                .GroupBy(static field => field.Name, StringComparer.OrdinalIgnoreCase)
                .ToDictionary(static group => group.Key, static group => group.OrderByDescending(field => field.Confidence).First(), StringComparer.OrdinalIgnoreCase);
            CheckField(report, fields, "SecurityCode", "920289");
            CheckField(report, fields, "ApplyCode", "920289");
            CheckField(report, fields, "ApplyDate", "2026-08-24");
            CheckField(report, fields, "IssuePrice", "17.71");
            CheckField(report, fields, "FundingMode", FundingMode.FullCash.ToString());
            var fundingEvidence = fields.GetValueOrDefault("FundingMode")?.Evidence;
            AddCheck(
                report,
                "bse-sample.full-cash-evidence",
                fundingEvidence?.Contains("全额", StringComparison.Ordinal) == true
                    || fundingEvidence?.Contains("足额", StringComparison.Ordinal) == true,
                EvidenceSummary(fundingEvidence));

            var sampleChecks = report.Checks.Skip(firstCheck).ToArray();
            return new BseSampleDiagnostic
            {
                Success = sampleChecks.All(static check => check.Passed),
                ReferenceCount = references.Count,
                SelectedTitle = selected.Title,
                SelectedUrl = selectedUrl,
                FileHash = document.FileHash,
                FileLength = fileBytes.LongLength,
                ExtractionStatus = document.ExtractionStatus.ToString(),
                ParserVersion = document.ParserVersion,
                Fields = document.ParsedFields.Select(ToParsedFieldDiagnostic).ToArray(),
            };
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            var error = Safe(ex.ToString(), workspace.DataRoot, 4000);
            AddCheck(report, "bse-sample.completed", false, ex.Message);
            return new BseSampleDiagnostic
            {
                Success = false,
                ReferenceCount = 0,
                Error = error,
            };
        }
    }

    private static EventDiagnostic ToEventDiagnostic(
        IpoEvent ipoEvent,
        IReadOnlyList<SourceFieldValue> fieldSources,
        IReadOnlyList<AnnouncementDocument> announcements,
        TemporaryWorkspace workspace) => new()
    {
        Id = ipoEvent.Id,
        Exchange = ipoEvent.Exchange.ToString(),
        Board = ipoEvent.Board.ToString(),
        SecurityCode = ipoEvent.SecurityCode,
        ApplyCode = ipoEvent.ApplyCode,
        LegacyCode = ipoEvent.LegacyCode,
        Name = ipoEvent.Name,
        ApplyDate = ipoEvent.ApplyDate?.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
        IssuePrice = ipoEvent.IssuePrice,
        LotSize = ipoEvent.LotSize,
        MaxApplyQuantity = ipoEvent.MaxApplyQuantity,
        IssueStatus = ipoEvent.Status.ToString(),
        LifecycleStatus = ipoEvent.LifecycleStatus.ToString(),
        DataQualityStatus = ipoEvent.DataQualityStatus.ToString(),
        AnnouncementUrl = SafeUri(ipoEvent.AnnouncementUrl),
        Sessions = ipoEvent.Sessions.Select(static session => new SessionDiagnostic
        {
            SessionNumber = session.SessionNumber,
            OfficialStart = session.OfficialStart.ToString("HH:mm", CultureInfo.InvariantCulture),
            OfficialEnd = session.OfficialEnd.ToString("HH:mm", CultureInfo.InvariantCulture),
            FundingMode = session.FundingMode.ToString(),
            AllocationTimeSensitive = session.AllocationTimeSensitive,
            Source = session.Source,
        }).ToArray(),
        FieldSources = fieldSources.Select(field => new FieldSourceDiagnostic
        {
            FieldName = field.FieldName,
            NormalizedValue = Safe(field.NormalizedValue, workspace.DataRoot, 500),
            Source = field.Source,
            Priority = field.Priority,
            FetchedAt = field.FetchedAt,
            RawHash = field.RawHash,
        }).ToArray(),
        Announcements = announcements.Select(document => ToAnnouncementDiagnostic(document, workspace)).ToArray(),
    };

    private static AnnouncementDiagnostic ToAnnouncementDiagnostic(AnnouncementDocument document, TemporaryWorkspace workspace)
    {
        var fileInfo = new FileInfo(document.LocalPath);
        return new AnnouncementDiagnostic
        {
            Provider = document.Reference.Provider,
            AnnouncementId = document.Reference.AnnouncementId,
            Title = Safe(document.Reference.Title, workspace.DataRoot, 500) ?? string.Empty,
            Url = WithoutQuery(document.Reference.Url),
            PublishedAt = document.Reference.PublishedAt,
            FileName = Path.GetFileName(document.LocalPath),
            FileLength = fileInfo.Exists && IsWithin(document.LocalPath, workspace.DataRoot) ? fileInfo.Length : 0,
            FileHash = document.FileHash,
            ExtractionStatus = document.ExtractionStatus.ToString(),
            ParserVersion = document.ParserVersion,
            Fields = document.ParsedFields.Select(ToParsedFieldDiagnostic).ToArray(),
        };
    }

    private static ParsedFieldDiagnostic ToParsedFieldDiagnostic(ParsedAnnouncementField field) => new()
    {
        Name = field.Name,
        Value = Safe(field.Value, dataRoot: null, 500) ?? string.Empty,
        Confidence = field.Confidence,
        EvidenceSummary = EvidenceSummary(field.Evidence),
        CharacterOffset = field.CharacterOffset,
    };

    private static void CheckField(
        DiagnosticReport report,
        IReadOnlyDictionary<string, ParsedAnnouncementField> fields,
        string name,
        string expected)
    {
        var actual = fields.GetValueOrDefault(name)?.Value;
        AddCheck(
            report,
            $"bse-sample.field.{name}",
            string.Equals(actual, expected, StringComparison.OrdinalIgnoreCase),
            $"expected={expected}; actual={actual ?? "<missing>"}");
    }

    private static string DescribeMissing(
        SourceDiagnostic?[] sources,
        Func<SourceDiagnostic?, bool> predicate)
    {
        var missing = ExpectedCollectors
            .Where((_, index) => !predicate(sources[index]))
            .ToArray();
        return missing.Length == 0
            ? "eastmoney、sse、cninfo、bse 均满足条件。"
            : $"未满足：{string.Join(", ", missing)}。";
    }

    private static string DescribeNamedMissing(
        string[] expectedNames,
        SourceDiagnostic?[] sources,
        Func<SourceDiagnostic?, bool> predicate)
    {
        var missing = expectedNames
            .Where((_, index) => !predicate(sources[index]))
            .ToArray();
        return missing.Length == 0
            ? expectedNames.Length == 0
                ? "本轮没有进入公告检查范围的事件。"
                : $"已尝试：{string.Join(", ", expectedNames)}。"
            : $"未尝试：{string.Join(", ", missing)}。";
    }

    private static bool IsAnnouncementScopeEvent(IpoEvent ipoEvent, DateOnly today) =>
        ipoEvent.ApplyDate is { } applyDate
        && applyDate >= today.AddDays(-7)
        && applyDate <= today.AddDays(45)
        && ipoEvent.Status is IssueStatus.Upcoming or IssueStatus.Active;

    private static bool HasUsableAnnouncementEvidence(AnnouncementDocument document) =>
        document.ExtractionStatus == ExtractionStatus.Extracted
        && File.Exists(document.LocalPath)
        && document.ParsedFields.Any(static field => field.Confidence >= 0.90m);

    private static string? AnnouncementProviderFor(Exchange exchange) => exchange switch
    {
        Exchange.Shanghai => "sse-announcement",
        Exchange.Shenzhen => "cninfo-announcement",
        Exchange.Beijing => "bse-announcement",
        _ => null,
    };

    private static Exchange? ExchangeForAnnouncementProvider(string provider) => provider switch
    {
        "sse-announcement" => Exchange.Shanghai,
        "cninfo-announcement" => Exchange.Shenzhen,
        "bse-announcement" => Exchange.Beijing,
        _ => null,
    };

    private static async Task<bool> HasValidPdfSignatureAsync(
        string path,
        TemporaryWorkspace workspace,
        CancellationToken cancellationToken)
    {
        if (!IsWithin(path, workspace.DataRoot) || !File.Exists(path))
        {
            return false;
        }

        var header = new byte[5];
        await using var stream = new FileStream(
            path,
            new FileStreamOptions
            {
                Mode = FileMode.Open,
                Access = FileAccess.Read,
                Share = FileShare.ReadWrite | FileShare.Delete,
                Options = FileOptions.Asynchronous | FileOptions.SequentialScan,
            });
        var read = await stream.ReadAsync(header, cancellationToken).ConfigureAwait(false);
        return read == header.Length && header.AsSpan().SequenceEqual("%PDF-"u8);
    }

    private static bool IsSafelyRedacted(string? value, string dataRoot)
    {
        if (string.IsNullOrEmpty(value))
        {
            return true;
        }

        var workingDirectory = Path.GetFullPath(Environment.CurrentDirectory);
        if (value.Contains(dataRoot, StringComparison.OrdinalIgnoreCase)
            || value.Contains(workingDirectory, StringComparison.OrdinalIgnoreCase)
            || value.Contains("Cookie:", StringComparison.OrdinalIgnoreCase)
            || value.Contains("Authorization:", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        return value.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
            .Where(static token => token.Contains("http://", StringComparison.OrdinalIgnoreCase)
                || token.Contains("https://", StringComparison.OrdinalIgnoreCase))
            .All(static token => !token.Contains('?', StringComparison.Ordinal)
                || token.Contains("?<redacted>", StringComparison.Ordinal));
    }

    private static async Task<string> CheckDatabaseIntegrityAsync(string databasePath, CancellationToken cancellationToken)
    {
        await using var connection = new SqliteConnection($"Data Source={databasePath};Mode=ReadWrite;");
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "PRAGMA integrity_check;";
        return Convert.ToString(await command.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false), CultureInfo.InvariantCulture)
            ?? "no-result";
    }

    private static void AddCheck(DiagnosticReport report, string name, bool passed, string detail) =>
        report.Checks.Add(new DiagnosticCheck
        {
            Name = name,
            Passed = passed,
            Detail = Safe(detail, dataRoot: null, 1500) ?? string.Empty,
        });

    private static string EvidenceSummary(string? value)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return string.Empty;
        }

        var compact = string.Join(' ', value.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
        return Safe(compact, dataRoot: null, 220) ?? string.Empty;
    }

    private static string? Safe(string? value, string? dataRoot, int maximumLength)
    {
        if (value is null)
        {
            return null;
        }

        var safe = DiagnosticRedactor.Redact(value);
        if (!string.IsNullOrEmpty(dataRoot))
        {
            safe = safe.Replace(dataRoot, "<temporary-data-root>", StringComparison.OrdinalIgnoreCase);
        }

        var workingDirectory = Path.GetFullPath(Environment.CurrentDirectory);
        safe = safe.Replace(workingDirectory, "<working-directory>", StringComparison.OrdinalIgnoreCase);
        safe = safe.Replace(
            workingDirectory.Replace(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar),
            "<working-directory>",
            StringComparison.OrdinalIgnoreCase);

        return safe.Length <= maximumLength ? safe : safe[..maximumLength] + "…";
    }

    private static string? SafeUri(string? value)
    {
        if (!Uri.TryCreate(value, UriKind.Absolute, out var uri))
        {
            return Safe(value, dataRoot: null, 500);
        }

        return WithoutQuery(uri);
    }

    private static string WithoutQuery(Uri uri) => uri.GetLeftPart(UriPartial.Path);

    private static bool IsWithin(string path, string root)
    {
        var fullPath = Path.GetFullPath(path);
        var fullRoot = Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        return fullPath.StartsWith(fullRoot, StringComparison.OrdinalIgnoreCase);
    }

    private sealed class NullReminderSink : IReminderSink
    {
        public Task ShowAsync(ReminderDelivery reminder, CancellationToken cancellationToken) => Task.CompletedTask;

        public Task ShowHealthSummaryAsync(HealthSummary summary, CancellationToken cancellationToken) => Task.CompletedTask;
    }
}

public static class DiagnosticOutput
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.CamelCase) },
    };

    public static async Task WriteAsync(DiagnosticReport report, string? outputPath)
    {
        var json = JsonSerializer.Serialize(report, JsonOptions);
        Console.WriteLine(json);
        if (outputPath is null)
        {
            return;
        }

        var directory = Path.GetDirectoryName(outputPath);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        await File.WriteAllTextAsync(outputPath, json + Environment.NewLine).ConfigureAwait(false);
    }
}

public sealed class TemporaryWorkspace
{
    private const string ParentDirectoryName = "StockIpoReminder.Diagnostics";

    private TemporaryWorkspace(string dataRoot)
    {
        DataRoot = dataRoot;
    }

    public string DataRoot { get; }
    public string DirectoryId => Path.GetFileName(DataRoot);
    public string DatabasePath => Path.Combine(DataRoot, "stock-ipo-reminder.db");

    public bool IsSafelyIsolated
    {
        get
        {
            var formalRoot = Path.GetFullPath(Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "StockIpoReminder"));
            var fullRoot = Path.GetFullPath(DataRoot);
            var tempRoot = Path.GetFullPath(Path.GetTempPath());
            return fullRoot.StartsWith(tempRoot, StringComparison.OrdinalIgnoreCase)
                && !fullRoot.StartsWith(formalRoot, StringComparison.OrdinalIgnoreCase);
        }
    }

    public static TemporaryWorkspace Create()
    {
        var parent = Path.Combine(Path.GetTempPath(), ParentDirectoryName);
        var root = Path.Combine(parent, $"run-{Guid.NewGuid():N}");
        Directory.CreateDirectory(root);
        return new TemporaryWorkspace(Path.GetFullPath(root));
    }

    public async Task<(bool Success, string? Error)> DeleteAsync()
    {
        var fullRoot = Path.GetFullPath(DataRoot);
        var expectedParent = Path.GetFullPath(Path.Combine(Path.GetTempPath(), ParentDirectoryName))
            .TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        if (!fullRoot.StartsWith(expectedParent, StringComparison.OrdinalIgnoreCase)
            || !Path.GetFileName(fullRoot).StartsWith("run-", StringComparison.Ordinal))
        {
            return (false, "拒绝删除不在诊断临时父目录内的路径。");
        }

        Exception? lastError = null;
        foreach (var delay in new[] { 0, 100, 300, 1000 })
        {
            if (delay > 0)
            {
                await Task.Delay(delay).ConfigureAwait(false);
            }

            try
            {
                if (Directory.Exists(fullRoot))
                {
                    Directory.Delete(fullRoot, recursive: true);
                }

                return (!Directory.Exists(fullRoot), null);
            }
            catch (IOException ex)
            {
                lastError = ex;
            }
            catch (UnauthorizedAccessException ex)
            {
                lastError = ex;
            }
        }

        return (false, DiagnosticRedactor.Redact(lastError?.Message));
    }
}
