using System.IO;
using System.Reflection;
using System.Text.Json;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class UiSmokeRunner
{
    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = true };
    private readonly ApplicationRuntimeOptions _runtimeOptions;
    private readonly IIpoRepository _repository;
    private readonly ReminderManagementService _managementService;
    private readonly DesktopReminderSink _reminderSink;

    public UiSmokeRunner(
        ApplicationRuntimeOptions runtimeOptions,
        IIpoRepository repository,
        ReminderManagementService managementService,
        DesktopReminderSink reminderSink)
    {
        _runtimeOptions = runtimeOptions;
        _repository = repository;
        _managementService = managementService;
        _reminderSink = reminderSink;
    }

    public async Task<bool> RunAsync(MainWindow mainWindow, CancellationToken cancellationToken = default)
    {
        if (_runtimeOptions.UiSmokeReport is null)
        {
            throw new InvalidOperationException("UI smoke 报告路径未配置。");
        }

        var reportDirectory = Path.GetDirectoryName(_runtimeOptions.UiSmokeReport)
            ?? throw new InvalidOperationException("UI smoke 报告目录无效。");
        Directory.CreateDirectory(reportDirectory);
        var checks = new Dictionary<string, bool>(StringComparer.Ordinal);
        var screenshots = new Dictionary<string, string>(StringComparer.Ordinal);
        string? error = null;
        ReminderWindow? reminderWindow = null;
        EventDetailsWindow? detailsWindow = null;
        HealthSummaryWindow? healthWindow = null;
        IReadOnlyList<IpoEvent> finalEvents = [];

        try
        {
            await WaitUntilLoadedAsync(mainWindow, cancellationToken);
            await mainWindow.RefreshForSmokeAsync(loadSettings: true);
            await WaitForRenderAsync(mainWindow);
            await mainWindow.Dispatcher.InvokeAsync(static () => { }, DispatcherPriority.Background, cancellationToken);

            Check(checks, "mainWindowVisible", mainWindow.IsVisible && mainWindow.IsLoaded);
            Check(checks, "trayIconVisible", _reminderSink.TrayVisible);
            Check(checks, "trayStatusShowsDataWarning", _reminderSink.TrayStatusText?.Contains("数据异常", StringComparison.Ordinal) == true);
            Check(checks, "mainWindowUsesRuntimeDataRoot", string.Equals(mainWindow.DataRoot, _runtimeOptions.DataRoot, StringComparison.OrdinalIgnoreCase));
            Check(checks, "mainWindowDisplaysRuntimeDataRoot", mainWindow.DataPathText.Text.Contains(_runtimeOptions.DataRoot, StringComparison.OrdinalIgnoreCase));
            Check(checks, "todayShowsThreeTasks", mainWindow.TodayItems.Items.Count == 3 && mainWindow.TodayCountText.Text == "3");
            Check(checks, "threeTasksStartPending", mainWindow.PendingCountText.Text == "3" && mainWindow.AcknowledgedCountText.Text == "0");
            var mainText = VisibleText(mainWindow);
            Check(checks, "mainWindowShowsShanghaiTask", mainText.Contains("沪测科技", StringComparison.Ordinal) && mainText.Contains("沪市", StringComparison.Ordinal));
            Check(checks, "mainWindowShowsShenzhenTask", mainText.Contains("深测股份", StringComparison.Ordinal) && mainText.Contains("深市", StringComparison.Ordinal));
            Check(checks, "mainWindowShowsBeijingTask", mainText.Contains("北测创新", StringComparison.Ordinal) && mainText.Contains("北交所", StringComparison.Ordinal));
            Check(checks, "mainWindowShowsIncompleteDataReview", mainText.Contains("待人工核验", StringComparison.Ordinal));
            Check(checks, "missingApplyCodeTriggersVisibleReviewWarning", mainText.Contains("申购代码仍缺失", StringComparison.Ordinal) && mainText.Contains("立即人工核验", StringComparison.Ordinal));
            Check(checks, "mainWindowShowsBeijingFundingWarning", mainText.Contains("全额缴付申购资金", StringComparison.Ordinal) && mainText.Contains("余股顺序", StringComparison.Ordinal));
            screenshots["mainBeforeConfirmation"] = await CaptureAsync(mainWindow, Path.Combine(reportDirectory, "main-before-confirmation.png"));

            mainWindow.SelectFutureForSmoke();
            await WaitForRenderAsync(mainWindow);
            var futureText = VisibleText(mainWindow);
            Check(checks, "futureCalendarShowsFourStateScenarios", mainWindow.FutureItems.Items.Count == 4);
            Check(checks, "futureCalendarShowsPostponedStatus", futureText.Contains("延期样本", StringComparison.Ordinal) && futureText.Contains("延期发行", StringComparison.Ordinal));
            Check(checks, "futureCalendarShowsSuspendedStatus", futureText.Contains("暂缓样本", StringComparison.Ordinal) && futureText.Contains("暂缓发行", StringComparison.Ordinal));
            Check(checks, "futureCalendarShowsTerminatedStatus", futureText.Contains("终止样本", StringComparison.Ordinal) && futureText.Contains("终止发行", StringComparison.Ordinal));
            Check(checks, "rescheduledAcknowledgementRequiresVisibleReview", futureText.Contains("改期重确认样本", StringComparison.Ordinal)
                && futureText.Contains("数据变更·需重确认", StringComparison.Ordinal)
                && futureText.Contains("旧确认已失效", StringComparison.Ordinal)
                && futureText.Contains("重新确认", StringComparison.Ordinal));
            var reviewBeforeConfirmation = await RequireEventAsync(UiSmokeScenarioSeeder.RescheduledReviewEventId, cancellationToken);
            Check(checks, "rescheduledEventInvalidatesOldAcknowledgement", reviewBeforeConfirmation.EventVersion >= 2
                && reviewBeforeConfirmation.LifecycleStatus == IpoLifecycleStatus.AcknowledgedNeedsReview);
            screenshots["futureStatusAndReview"] = await CaptureAsync(mainWindow, Path.Combine(reportDirectory, "future-status-and-review.png"));

            await mainWindow.AcknowledgeForSmokeAsync(UiSmokeScenarioSeeder.RescheduledReviewEventId);
            await WaitForRenderAsync(mainWindow);
            var reviewAfterConfirmation = await RequireEventAsync(UiSmokeScenarioSeeder.RescheduledReviewEventId, cancellationToken);
            Check(checks, "changedEventCanBeAcknowledgedAgain", reviewAfterConfirmation.LifecycleStatus == IpoLifecycleStatus.Acknowledged);
            screenshots["futureAfterReconfirmation"] = await CaptureAsync(mainWindow, Path.Combine(reportDirectory, "future-after-reconfirmation.png"));
            mainWindow.SelectTodayForSmoke();
            await WaitForRenderAsync(mainWindow);

            var settings = await _managementService.GetSettingsAsync(cancellationToken);
            var beijing = await RequireEventAsync(UiSmokeScenarioSeeder.BeijingEventId, cancellationToken);
            var lifecycleBeforeReminderClose = beijing.LifecycleStatus;
            var reminderDelivery = new ReminderDelivery
            {
                OutboxId = 0,
                Event = beijing,
                DueAt = DateTimeOffset.Now,
                Level = ReminderLevel.Hourly,
                DedupeKey = "ui-smoke-reminder",
            };
            reminderWindow = new ReminderWindow(reminderDelivery, settings, _managementService);
            reminderWindow.ShowWithoutActivation();
            await WaitUntilLoadedAsync(reminderWindow, cancellationToken);
            await WaitForRenderAsync(reminderWindow);
            Check(checks, "reminderShowsHourlyLevel", reminderWindow.LevelText.Text == "待确认"
                && IsBrushColor(reminderWindow.LevelBadge.Background, 29, 78, 216));
            reminderWindow.UpdateReminder(reminderDelivery with { Level = ReminderLevel.FifteenMinutes }, settings);
            await WaitForRenderAsync(reminderWindow);
            Check(checks, "reminderEscalatesToFifteenMinutes", reminderWindow.LevelText.Text == "临近截止"
                && IsBrushColor(reminderWindow.LevelBadge.Background, 29, 78, 216));
            reminderWindow.UpdateReminder(reminderDelivery with { Level = ReminderLevel.FiveMinutes }, settings);
            await WaitForRenderAsync(reminderWindow);
            Check(checks, "reminderEscalatesToFiveMinutes", reminderWindow.LevelText.Text == "高频提醒"
                && IsBrushColor(reminderWindow.LevelBadge.Background, 185, 28, 28));
            reminderWindow.UpdateReminder(reminderDelivery with { Level = ReminderLevel.TwoMinutes }, settings);
            await WaitForRenderAsync(reminderWindow);
            Check(checks, "reminderEscalatesToTwoMinutes", reminderWindow.LevelText.Text == "紧急提醒"
                && IsBrushColor(reminderWindow.LevelBadge.Background, 185, 28, 28));
            Check(checks, "toastUnavailableInSmoke", !_reminderSink.ToastAvailable);
            Check(checks, "inAppReminderVisibleWhenToastUnavailable", reminderWindow.IsVisible && !_reminderSink.ToastAvailable);
            Check(checks, "reminderShowsBeijingFundingWarning", reminderWindow.WarningText.Text.Contains("全额缴付申购资金", StringComparison.Ordinal));
            Check(checks, "reminderShowsManualReviewWarning", reminderWindow.WarningText.Text.Contains("待人工核验", StringComparison.Ordinal));
            Check(checks, "reminderRequiresExplicitConfirmation", VisibleText(reminderWindow).Contains("只有点击“确认已申购”", StringComparison.Ordinal));
            screenshots["beijingReminder"] = await CaptureAsync(reminderWindow, Path.Combine(reportDirectory, "beijing-reminder.png"));
            reminderWindow.Close();
            reminderWindow = null;
            await mainWindow.Dispatcher.InvokeAsync(static () => { }, DispatcherPriority.Background, cancellationToken);
            var afterReminderClose = await RequireEventAsync(UiSmokeScenarioSeeder.BeijingEventId, cancellationToken);
            Check(checks, "closingReminderDoesNotConfirm", afterReminderClose.LifecycleStatus == lifecycleBeforeReminderClose);

            var shanghai = await RequireEventAsync(UiSmokeScenarioSeeder.ShanghaiEventId, cancellationToken);
            detailsWindow = new EventDetailsWindow(shanghai.Id, _managementService) { Owner = mainWindow };
            detailsWindow.Show();
            await WaitUntilLoadedAsync(detailsWindow, cancellationToken);
            await detailsWindow.RefreshForSmokeAsync();
            await WaitForRenderAsync(detailsWindow);
            Check(checks, "detailsShowsFieldSources", detailsWindow.FieldSourceList.Items.Count >= 7);
            Check(checks, "detailsShowsAnnouncementEvidence", detailsWindow.AnnouncementItems.Items.Count >= 1);
            Check(checks, "detailsShowsVerifiedQuality", detailsWindow.QualityText.Text.Contains("公告已核验", StringComparison.Ordinal));
            Check(checks, "detailsShowsEventIdentity", detailsWindow.TitleText.Text.Contains("沪测科技", StringComparison.Ordinal) && detailsWindow.SummaryText.Text.Contains("688001", StringComparison.Ordinal));
            var firstFieldItem = detailsWindow.FieldSourceList.ItemContainerGenerator.ContainerFromIndex(0) as System.Windows.Controls.ListViewItem;
            var firstFieldHeader = FindVisualChild<System.Windows.Controls.GridViewColumnHeader>(detailsWindow.FieldSourceList);
            Check(
                checks,
                "detailsFieldSourcesUseReadableDarkTheme",
                IsBrushColor(firstFieldItem?.Foreground, 248, 250, 252)
                && IsBrushColor(firstFieldHeader?.Foreground, 248, 250, 252)
                && IsBrushColor(firstFieldHeader?.Background, 23, 35, 58));
            screenshots["detailsFieldSources"] = await CaptureAsync(detailsWindow, Path.Combine(reportDirectory, "details-field-sources.png"));
            detailsWindow.DetailsTabs.SelectedIndex = 1;
            await WaitForRenderAsync(detailsWindow);
            Check(checks, "detailsAnnouncementTabShowsParsedEvidence", VisibleText(detailsWindow).Contains("发行价格=18.88", StringComparison.Ordinal));
            screenshots["detailsAnnouncement"] = await CaptureAsync(detailsWindow, Path.Combine(reportDirectory, "details-announcement.png"));

            await _managementService.ApplyManualOverrideAsync(
                shanghai.Id,
                shanghai.EventVersion,
                "MaxApplyQuantity",
                "13000",
                "UI smoke 已核对正式发行公告",
                UiSmokeScenarioSeeder.ShanghaiAnnouncementDocumentId,
                cancellationToken);
            await detailsWindow.RefreshForSmokeAsync();
            detailsWindow.DetailsTabs.SelectedIndex = 2;
            await WaitForRenderAsync(detailsWindow);
            var overriddenShanghai = await RequireEventAsync(UiSmokeScenarioSeeder.ShanghaiEventId, cancellationToken);
            var overrideText = VisibleText(detailsWindow);
            Check(checks, "manualOverrideChangesEffectiveField", overriddenShanghai.HasManualOverride
                && overriddenShanghai.MaxApplyQuantity == 13_000
                && overriddenShanghai.ManualOverrideFields.Contains("MaxApplyQuantity", StringComparer.OrdinalIgnoreCase));
            Check(checks, "manualOverrideKeepsOriginalFieldSources", detailsWindow.FieldSourceList.Items.Count >= 7);
            Check(checks, "manualOverrideAuditIsVisible", detailsWindow.OverrideItems.Items.Count == 1
                && overrideText.Contains("申购上限 = 13000", StringComparison.Ordinal)
                && overrideText.Contains("UI smoke 已核对正式发行公告", StringComparison.Ordinal));
            Check(checks, "manualOverrideLinksOfficialAnnouncement", overrideText.Contains("依据公告：沪测科技首次公开发行股票并在科创板上市发行公告", StringComparison.Ordinal));
            screenshots["detailsManualOverride"] = await CaptureAsync(detailsWindow, Path.Combine(reportDirectory, "details-manual-override.png"));
            detailsWindow.Close();
            detailsWindow = null;

            await mainWindow.AcknowledgeForSmokeAsync(UiSmokeScenarioSeeder.ShanghaiEventId);
            await WaitForRenderAsync(mainWindow);
            var acknowledgedShanghai = await RequireEventAsync(UiSmokeScenarioSeeder.ShanghaiEventId, cancellationToken);
            var stillPendingShenzhen = await RequireEventAsync(UiSmokeScenarioSeeder.ShenzhenEventId, cancellationToken);
            var stillPendingBeijing = await RequireEventAsync(UiSmokeScenarioSeeder.BeijingEventId, cancellationToken);
            Check(checks, "oneTaskCanBeAcknowledged", acknowledgedShanghai.LifecycleStatus == IpoLifecycleStatus.Acknowledged);
            Check(checks, "acknowledgingOneDoesNotAcknowledgeOthers", IsPending(stillPendingShenzhen) && IsPending(stillPendingBeijing));
            Check(checks, "mainCountsReflectIndependentAcknowledgement", mainWindow.AcknowledgedCountText.Text == "1" && mainWindow.PendingCountText.Text == "2");
            screenshots["mainAfterConfirmation"] = await CaptureAsync(mainWindow, Path.Combine(reportDirectory, "main-after-confirmation.png"));

            var health = await _managementService.GetHealthSummaryAsync(cancellationToken);
            healthWindow = new HealthSummaryWindow(health);
            healthWindow.ShowWithoutActivation();
            await WaitUntilLoadedAsync(healthWindow, cancellationToken);
            await WaitForRenderAsync(healthWindow);
            Check(checks, "healthSummaryShowsTaskCounts", healthWindow.SummaryText.Text.Contains("今日任务 3 只", StringComparison.Ordinal) && healthWindow.SummaryText.Text.Contains("待确认 2 只", StringComparison.Ordinal));
            Check(checks, "healthSummaryShowsSources", healthWindow.SourceText.Text.Contains("eastmoney", StringComparison.OrdinalIgnoreCase) && healthWindow.SourceText.Text.Contains("bse", StringComparison.OrdinalIgnoreCase));
            Check(
                checks,
                "healthSummaryShowsWarningState",
                health.OverallState == HealthState.Warning
                && healthWindow.TitleText.Text == "提醒系统需要检查"
                && IsBrushColor(healthWindow.OuterBorder.BorderBrush, 245, 158, 11));
            screenshots["healthSummary"] = await CaptureAsync(healthWindow, Path.Combine(reportDirectory, "health-summary.png"));
            healthWindow.Close();
            healthWindow = null;

            mainWindow.SelectHealthForSmoke();
            await WaitForRenderAsync(mainWindow);
            Check(checks, "mainHealthPageShowsHealthState", mainWindow.HealthSummaryText.Text.Contains("今日任务 3 只", StringComparison.Ordinal));
            screenshots["mainHealthPage"] = await CaptureAsync(mainWindow, Path.Combine(reportDirectory, "main-health-page.png"));

            mainWindow.Close();
            await mainWindow.Dispatcher.InvokeAsync(static () => { }, DispatcherPriority.Background, cancellationToken);
            Check(checks, "closingMainWindowHidesToTray", !mainWindow.IsVisible && !mainWindow.ShowInTaskbar);
        }
        catch (Exception ex)
        {
            error = DescribeException(ex, reportDirectory);
            Check(checks, "runnerCompletedWithoutException", false);
        }
        finally
        {
            CloseWindow(healthWindow);
            CloseWindow(detailsWindow);
            CloseWindow(reminderWindow);
            try
            {
                var today = ChinaTime.Today(TimeProvider.System);
                finalEvents = await _repository.GetEventsAsync(today, today.AddDays(60), cancellationToken);
            }
            catch (Exception ex)
            {
                error ??= DescribeException(ex, reportDirectory);
                Check(checks, "finalStateReadable", false);
            }
        }

        foreach (var path in screenshots.Values)
        {
            Check(checks, $"screenshotExists:{Path.GetFileNameWithoutExtension(path)}", File.Exists(path) && new FileInfo(path).Length > 1_024);
        }

        if (!checks.ContainsKey("runnerCompletedWithoutException"))
        {
            Check(checks, "runnerCompletedWithoutException", true);
        }

        var failedChecks = checks.Where(static pair => !pair.Value).Select(static pair => pair.Key).ToArray();
        var reportScreenshots = screenshots.ToDictionary(
            static pair => pair.Key,
            pair => Path.GetRelativePath(reportDirectory, pair.Value).Replace('\\', '/'),
            StringComparer.Ordinal);
        var report = new
        {
            success = failedChecks.Length == 0,
            version = Assembly.GetEntryAssembly()?.GetName().Version?.ToString(3) ?? "unknown",
            scenarioVersion = "2",
            generatedAtUtc = DateTimeOffset.UtcNow,
            os = new
            {
                description = Environment.OSVersion.VersionString,
                version = Environment.OSVersion.Version.ToString(),
                is64Bit = Environment.Is64BitOperatingSystem,
            },
            dataRoot = "<isolated-smoke-data-root>",
            dataRootInstance = _runtimeOptions.InstanceKey[..12],
            toastAvailable = _reminderSink.ToastAvailable,
            checks,
            failedChecks,
            screenshots = reportScreenshots,
            finalEvents = finalEvents.Select(static ipoEvent => new
            {
                ipoEvent.Id,
                ipoEvent.Name,
                exchange = ipoEvent.Exchange.ToString(),
                lifecycle = ipoEvent.LifecycleStatus.ToString(),
                quality = ipoEvent.DataQualityStatus.ToString(),
                ipoEvent.EventVersion,
            }),
            error,
        };
        await WriteReportAsync(_runtimeOptions.UiSmokeReport, report, cancellationToken);
        return failedChecks.Length == 0;
    }

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

    private async Task<IpoEvent> RequireEventAsync(string eventId, CancellationToken cancellationToken) =>
        await _repository.GetEventAsync(eventId, cancellationToken)
        ?? throw new InvalidOperationException($"UI smoke 任务不存在：{eventId}");

    private static async Task WaitUntilLoadedAsync(Window window, CancellationToken cancellationToken)
    {
        if (window.IsLoaded)
        {
            return;
        }

        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void Loaded(object? sender, RoutedEventArgs args) => completion.TrySetResult();
        window.Loaded += Loaded;
        try
        {
            await completion.Task.WaitAsync(TimeSpan.FromSeconds(15), cancellationToken);
        }
        finally
        {
            window.Loaded -= Loaded;
        }
    }

    private static async Task WaitForRenderAsync(Window window)
    {
        await window.Dispatcher.InvokeAsync(window.UpdateLayout, DispatcherPriority.Loaded);
        await window.Dispatcher.InvokeAsync(static () => { }, DispatcherPriority.Render);
    }

    private static async Task<string> CaptureAsync(Window window, string path)
    {
        await WaitForRenderAsync(window);
        var captureTarget = window.Content as FrameworkElement ?? window;
        var dpi = VisualTreeHelper.GetDpi(captureTarget);
        var width = Math.Max(1, (int)Math.Ceiling(captureTarget.ActualWidth * dpi.DpiScaleX));
        var height = Math.Max(1, (int)Math.Ceiling(captureTarget.ActualHeight * dpi.DpiScaleY));
        var bitmap = new RenderTargetBitmap(width, height, 96 * dpi.DpiScaleX, 96 * dpi.DpiScaleY, PixelFormats.Pbgra32);
        var drawing = new DrawingVisual();
        using (var context = drawing.RenderOpen())
        {
            var bounds = new Rect(0, 0, captureTarget.ActualWidth, captureTarget.ActualHeight);
            context.DrawRectangle(window.Background, null, bounds);
            context.DrawRectangle(new VisualBrush(captureTarget), null, bounds);
        }

        bitmap.Render(drawing);
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bitmap));
        await using var stream = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.Read);
        encoder.Save(stream);
        return path;
    }

    private static string VisibleText(DependencyObject root)
    {
        var values = new List<string>();
        CollectText(root, values);
        return string.Join('\n', values);
    }

    private static void CollectText(DependencyObject current, ICollection<string> values)
    {
        if (current is System.Windows.Controls.TextBlock { Visibility: Visibility.Visible } textBlock
            && !string.IsNullOrWhiteSpace(textBlock.Text))
        {
            values.Add(textBlock.Text);
        }

        var count = VisualTreeHelper.GetChildrenCount(current);
        for (var index = 0; index < count; index++)
        {
            CollectText(VisualTreeHelper.GetChild(current, index), values);
        }
    }

    private static bool IsPending(IpoEvent ipoEvent) => ipoEvent.LifecycleStatus is
        IpoLifecycleStatus.Scheduled or IpoLifecycleStatus.ActiveUnconfirmed or IpoLifecycleStatus.AcknowledgedNeedsReview;

    private static T? FindVisualChild<T>(DependencyObject root)
        where T : DependencyObject
    {
        var count = VisualTreeHelper.GetChildrenCount(root);
        for (var index = 0; index < count; index++)
        {
            var child = VisualTreeHelper.GetChild(root, index);
            if (child is T match)
            {
                return match;
            }

            var nested = FindVisualChild<T>(child);
            if (nested is not null)
            {
                return nested;
            }
        }

        return null;
    }

    private static bool IsBrushColor(System.Windows.Media.Brush? brush, byte red, byte green, byte blue) =>
        brush is SolidColorBrush solid
        && solid.Color.R == red
        && solid.Color.G == green
        && solid.Color.B == blue;

    private static void Check(Dictionary<string, bool> checks, string name, bool value) => checks[name] = value;

    private static void CloseWindow(Window? window)
    {
        if (window is null)
        {
            return;
        }

        try
        {
            window.Close();
        }
        catch (InvalidOperationException)
        {
        }
    }

    private static async Task WriteReportAsync(string path, object report, CancellationToken cancellationToken)
    {
        var temporaryPath = path + $".{Guid.NewGuid():N}.tmp";
        await File.WriteAllTextAsync(temporaryPath, JsonSerializer.Serialize(report, JsonOptions), cancellationToken);
        File.Move(temporaryPath, path, overwrite: true);
    }
}
