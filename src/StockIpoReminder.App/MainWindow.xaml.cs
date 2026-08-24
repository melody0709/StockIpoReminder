using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Runtime;
using MediaBrush = System.Windows.Media.Brush;
using MessageBox = System.Windows.MessageBox;
using WpfButton = System.Windows.Controls.Button;

namespace StockIpoReminder.App;

public partial class MainWindow : Window
{
    private static readonly CultureInfo ChineseCulture = CultureInfo.GetCultureInfo("zh-CN");
    private readonly ReminderManagementService _managementService;
    private readonly RuntimeState _runtimeState;
    private readonly AutoStartService _autoStartService;
    private readonly DesktopReminderSink _reminderSink;
    private readonly DiagnosticBundleService _diagnosticBundleService;
    private readonly DispatcherTimer _refreshTimer;
    private readonly string _dataRoot;
    private readonly Dictionary<string, EventDetailsWindow> _detailWindows = new(StringComparer.OrdinalIgnoreCase);
    private AppSettings _settings = new();
    private HealthSummary? _healthSummary;
    private bool _loaded;

    public MainWindow(
        ReminderManagementService managementService,
        RuntimeState runtimeState,
        AutoStartService autoStartService,
        DesktopReminderSink reminderSink,
        DiagnosticBundleService diagnosticBundleService,
        ApplicationRuntimeOptions runtimeOptions)
    {
        InitializeComponent();
        _managementService = managementService;
        _runtimeState = runtimeState;
        _autoStartService = autoStartService;
        _reminderSink = reminderSink;
        _diagnosticBundleService = diagnosticBundleService;
        _dataRoot = runtimeOptions.DataRoot;
        _refreshTimer = new DispatcherTimer(TimeSpan.FromSeconds(30), DispatcherPriority.Background, RefreshTimer_Tick, Dispatcher);
        _runtimeState.Changed += RuntimeState_Changed;
        Closed += (_, _) =>
        {
            _refreshTimer.Stop();
            _runtimeState.Changed -= RuntimeState_Changed;
        };
    }

    public void ShowAndActivate()
    {
        if (!IsVisible)
        {
            Show();
        }

        if (WindowState == WindowState.Minimized)
        {
            WindowState = WindowState.Normal;
        }

        ShowInTaskbar = true;
        Activate();
        Topmost = true;
        Topmost = false;
        Focus();
    }

    public void ShowSettings()
    {
        ShowAndActivate();
        MainTabs.SelectedItem = SettingsTab;
    }

    internal string DataRoot => _dataRoot;

    internal Task RefreshForSmokeAsync(bool loadSettings = true) => RefreshAsync(loadSettings);

    internal async Task AcknowledgeForSmokeAsync(string eventId)
    {
        var details = await _managementService.GetEventDetailsAsync(eventId);
        await _managementService.AcknowledgeAsync(details.Event.Id, details.Event.EventVersion);
        await RefreshAsync(loadSettings: false);
    }

    internal void SelectHealthForSmoke() => MainTabs.SelectedIndex = 2;

    internal void SelectTodayForSmoke() => MainTabs.SelectedIndex = 0;

    internal void SelectFutureForSmoke() => MainTabs.SelectedIndex = 1;

    public void ShowEventDetails(string eventId)
    {
        ShowAndActivate();
        if (_detailWindows.TryGetValue(eventId, out var existing) && existing.IsLoaded)
        {
            existing.Activate();
            return;
        }

        var window = new EventDetailsWindow(eventId, _managementService) { Owner = this };
        window.EventChanged += async (_, _) => await RefreshAsync(loadSettings: false);
        window.Closed += (_, _) => _detailWindows.Remove(eventId);
        _detailWindows[eventId] = window;
        window.Show();
    }

    private async void Window_Loaded(object sender, RoutedEventArgs e)
    {
        if (_loaded)
        {
            return;
        }

        _loaded = true;
        DataPathText.Text = $"数据、公告缓存和日志目录：{_dataRoot}";
        ApplyRuntimeSnapshot(_runtimeState.Snapshot);
        await RefreshAsync(loadSettings: true);
        _refreshTimer.Start();

        if (!_settings.OnboardingCompleted)
        {
            ShowSettings();
        }
    }

    private void Window_Closing(object? sender, CancelEventArgs e)
    {
        if (System.Windows.Application.Current is App app && !app.IsExiting)
        {
            e.Cancel = true;
            Hide();
            ShowInTaskbar = false;
        }
    }

    private async void RefreshTimer_Tick(object? sender, EventArgs e) => await RefreshAsync(loadSettings: false);

    private void RuntimeState_Changed(object? sender, RuntimeSnapshot snapshot) =>
        Dispatcher.BeginInvoke(() => ApplyRuntimeSnapshot(snapshot));

    private void ApplyRuntimeSnapshot(RuntimeSnapshot snapshot)
    {
        RuntimeStatusText.Text = snapshot.StatusText;
        LastSyncText.Text = snapshot.LastSyncCompletedAt is null
            ? "尚未完成同步"
            : $"最近同步 {snapshot.LastSyncCompletedAt.Value.ToLocalTime():MM-dd HH:mm:ss}";
        SyncButton.IsEnabled = !snapshot.IsSynchronizing;
        RuntimeDot.Fill = snapshot.IsSynchronizing
            ? Brush("#F59E0B")
            : snapshot.LastSyncSucceeded == false
                ? Brush("#EF4444")
                : snapshot.LastSyncSucceeded == true
                    ? Brush("#22C55E")
                    : Brush("#F59E0B");
        ClockStatusText.Text = snapshot.Clock.Message;
        ClockStatusText.Foreground = snapshot.Clock.State switch
        {
            HealthState.Healthy => Brush("#86EFAC"),
            HealthState.Warning => Brush("#FCD34D"),
            HealthState.Failed => Brush("#FCA5A5"),
            _ => Brush("#94A3B8"),
        };
    }

    private async Task RefreshAsync(bool loadSettings)
    {
        try
        {
            if (loadSettings)
            {
                _settings = await _managementService.GetSettingsAsync();
                PopulateSettings(_settings);
            }

            var today = ChinaTime.Today(TimeProvider.System);
            var eventsTask = _managementService.GetEventsAsync(today, today.AddDays(60));
            var healthTask = _managementService.GetHealthSummaryAsync();
            await Task.WhenAll(eventsTask, healthTask);

            var events = eventsTask.Result
                .Where(ipoEvent => _settings.IsExchangeEnabled(ipoEvent.Exchange))
                .OrderBy(static ipoEvent => ipoEvent.ApplyDate)
                .ThenBy(static ipoEvent => ipoEvent.Exchange)
                .ThenBy(static ipoEvent => ipoEvent.SecurityCode, StringComparer.Ordinal)
                .ToArray();
            var todayEvents = events.Where(ipoEvent => ipoEvent.ApplyDate == today).ToArray();
            var futureEvents = events.Where(ipoEvent => ipoEvent.ApplyDate > today).ToArray();

            TodayItems.ItemsSource = todayEvents.Select(ipoEvent => new EventCardViewModel(ipoEvent, _settings, today)).ToArray();
            FutureItems.ItemsSource = futureEvents.Select(ipoEvent => new EventCardViewModel(ipoEvent, _settings, today)).ToArray();
            TodayEmptyText.Visibility = todayEvents.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
            FutureEmptyText.Visibility = futureEvents.Length == 0 ? Visibility.Visible : Visibility.Collapsed;

            TodayCountText.Text = todayEvents.Length.ToString(ChineseCulture);
            PendingCountText.Text = todayEvents.Count(IsPending).ToString(ChineseCulture);
            AcknowledgedCountText.Text = todayEvents.Count(static item => item.LifecycleStatus == IpoLifecycleStatus.Acknowledged).ToString(ChineseCulture);
            IssueCountText.Text = todayEvents.Count(static item => item.DataQualityStatus is DataQualityStatus.DataConflict
                or DataQualityStatus.Stale
                or DataQualityStatus.ManualReviewRequired).ToString(ChineseCulture);

            _healthSummary = healthTask.Result;
            PopulateHealth(_healthSummary);
            var clockState = _runtimeState.Snapshot.Clock.State;
            _reminderSink.UpdateTrayStatus(
                todayEvents.Count(IsPending),
                _healthSummary.OverallState != HealthState.Healthy
                || clockState is HealthState.Warning or HealthState.Failed);
        }
        catch (Exception ex)
        {
            RuntimeStatusText.Text = $"界面刷新失败：{ex.Message}";
            RuntimeDot.Fill = Brush("#EF4444");
        }
    }

    private void PopulateSettings(AppSettings settings)
    {
        ShanghaiEnabledCheck.IsChecked = settings.ShanghaiEnabled;
        ShenzhenEnabledCheck.IsChecked = settings.ShenzhenEnabled;
        BeijingEnabledCheck.IsChecked = settings.BeijingEnabled;
        ShanghaiStartText.Text = settings.ShanghaiBrokerAcceptStart.ToString("HH:mm", ChineseCulture);
        ShenzhenStartText.Text = settings.ShenzhenBrokerAcceptStart.ToString("HH:mm", ChineseCulture);
        BeijingStartText.Text = settings.BeijingBrokerAcceptStart.ToString("HH:mm", ChineseCulture);
        SafetyCutoffText.Text = settings.SafetyCutoff.ToString("HH:mm", ChineseCulture);
        BeijingReservationCheck.IsChecked = settings.BeijingReservationSupported;
        SoundCheck.IsChecked = settings.SoundEnabled;
        FlashCheck.IsChecked = settings.FlashTaskbar;
        ToastCheck.IsChecked = settings.ToastEnabled;
        HealthSummaryCheck.IsChecked = settings.DailyHealthSummaryEnabled;
        AutoStartCheck.IsChecked = settings.AutoStartEnabled;
        NormalSyncText.Text = settings.NormalSyncMinutes.ToString(ChineseCulture);
        ActiveSyncText.Text = settings.ActiveDaySyncMinutes.ToString(ChineseCulture);
        NotificationTestStatusText.Text = settings.NotificationSelfTestCompleted ? "提醒通道已测试" : "尚未完成提醒通道测试";
        FirstRunPanel.Visibility = settings.OnboardingCompleted ? Visibility.Collapsed : Visibility.Visible;
    }

    private void PopulateHealth(HealthSummary summary)
    {
        HealthTitleText.Text = summary.OverallState switch
        {
            HealthState.Healthy => "程序与数据源运行正常",
            HealthState.Warning => "存在待核验任务或异常数据源",
            HealthState.Failed => "提醒系统需要立即检查",
            _ => "健康状态尚未建立",
        };
        HealthTitleText.Foreground = summary.OverallState switch
        {
            HealthState.Healthy => Brush("#86EFAC"),
            HealthState.Warning => Brush("#FCD34D"),
            HealthState.Failed => Brush("#FCA5A5"),
            _ => Brush("#E2E8F0"),
        };
        HealthSummaryText.Text = $"今日任务 {summary.TodayTaskCount} 只，待确认 {summary.PendingConfirmationCount} 只，来源冲突 {summary.ConflictCount} 只，待人工核验 {summary.ManualReviewCount} 只。";
        HeartbeatText.Text = $"调度心跳 {FormatTimestamp(summary.SchedulerHeartbeat)} · 投递心跳 {FormatTimestamp(summary.DeliveryHeartbeat)}";
        SourceHealthItems.ItemsSource = summary.Sources.Select(static source => new SourceHealthViewModel(source)).ToArray();
    }

    private async void SyncButton_Click(object sender, RoutedEventArgs e)
    {
        _managementService.RequestSync("用户手动同步");
        SettingsStatusText.Text = "已请求同步";
        await Task.Delay(500);
        ApplyRuntimeSnapshot(_runtimeState.Snapshot);
    }

    private void HideButton_Click(object sender, RoutedEventArgs e)
    {
        Hide();
        ShowInTaskbar = false;
    }

    private async void RefreshButton_Click(object sender, RoutedEventArgs e) => await RefreshAsync(loadSettings: true);

    private async void ConfirmEvent_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not WpfButton { Tag: IpoEvent ipoEvent })
        {
            return;
        }

        var answer = MessageBox.Show(
            $"请确认你已经在券商客户端提交了 {ipoEvent.Name}（申购代码 {ipoEvent.DisplayCode}）的申购委托。\n\n本程序不会检查委托是否受理或成功。确认后，本申购日内停止该股票的重复提醒。",
            "二次确认：已经提交申购委托？",
            MessageBoxButton.YesNo,
            MessageBoxImage.Warning,
            MessageBoxResult.No);
        if (answer != MessageBoxResult.Yes)
        {
            return;
        }

        try
        {
            await _managementService.AcknowledgeAsync(ipoEvent.Id, ipoEvent.EventVersion);
            await RefreshAsync(loadSettings: false);
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "确认失败", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private async void RevokeEvent_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not WpfButton { Tag: IpoEvent ipoEvent })
        {
            return;
        }

        if (MessageBox.Show(
                $"撤销 {ipoEvent.Name} 的“已申购”确认后，程序会立即恢复后续提醒。确定撤销吗？",
                "撤销确认",
                MessageBoxButton.YesNo,
                MessageBoxImage.Warning,
                MessageBoxResult.No) != MessageBoxResult.Yes)
        {
            return;
        }

        try
        {
            await _managementService.RevokeAcknowledgementAsync(ipoEvent.Id, ipoEvent.EventVersion);
            await RefreshAsync(loadSettings: false);
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "撤销失败", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private void OpenDetails_Click(object sender, RoutedEventArgs e)
    {
        if (sender is WpfButton { Tag: IpoEvent ipoEvent })
        {
            ShowEventDetails(ipoEvent.Id);
        }
    }

    private async void TestNotification_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await _reminderSink.ShowNotificationTestAsync();
            var toastNote = _reminderSink.ToastAvailable
                ? "Windows Toast、托盘气泡、声音和置顶窗口"
                : "托盘气泡、声音和置顶窗口（当前系统 Toast 不可用，程序仍可可靠提醒）";
            var answer = MessageBox.Show(
                $"刚才已触发 {toastNote}。\n\n你是否已经看到并听到当前可用的提醒通道？",
                "确认提醒通道测试",
                MessageBoxButton.YesNo,
                MessageBoxImage.Question,
                MessageBoxResult.No);
            var completed = answer == MessageBoxResult.Yes;
            _settings = _settings with { NotificationSelfTestCompleted = completed };
            await _managementService.SaveSettingsAsync(_settings);
            NotificationTestStatusText.Text = completed
                ? "提醒通道测试已由你确认通过"
                : "测试未确认通过，请检查专注助手、通知权限和声音设置后重试";
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "提醒测试失败", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private async void SaveSettings_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            if (!TryParseTime(ShanghaiStartText.Text, out var shanghaiStart)
                || !TryParseTime(ShenzhenStartText.Text, out var shenzhenStart)
                || !TryParseTime(BeijingStartText.Text, out var beijingStart)
                || !TryParseTime(SafetyCutoffText.Text, out var cutoff))
            {
                throw new InvalidOperationException("时间格式必须为 HH:mm，例如 09:15 或 14:55。" );
            }

            if (cutoff < new TimeOnly(13, 0) || cutoff > new TimeOnly(15, 0))
            {
                throw new InvalidOperationException("安全截止时间应在 13:00 到 15:00 之间，建议使用 14:55。" );
            }

            if (!int.TryParse(NormalSyncText.Text, NumberStyles.Integer, ChineseCulture, out var normalMinutes)
                || !int.TryParse(ActiveSyncText.Text, NumberStyles.Integer, ChineseCulture, out var activeMinutes))
            {
                throw new InvalidOperationException("同步频率必须是整数分钟。" );
            }

            normalMinutes = Math.Clamp(normalMinutes, 5, 120);
            activeMinutes = Math.Clamp(activeMinutes, 5, 60);
            if (ShanghaiEnabledCheck.IsChecked != true
                && ShenzhenEnabledCheck.IsChecked != true
                && BeijingEnabledCheck.IsChecked != true)
            {
                throw new InvalidOperationException("至少需要启用一个市场。" );
            }

            var next = _settings with
            {
                ShanghaiEnabled = ShanghaiEnabledCheck.IsChecked == true,
                ShenzhenEnabled = ShenzhenEnabledCheck.IsChecked == true,
                BeijingEnabled = BeijingEnabledCheck.IsChecked == true,
                ShanghaiBrokerAcceptStart = shanghaiStart,
                ShenzhenBrokerAcceptStart = shenzhenStart,
                BeijingBrokerAcceptStart = beijingStart,
                SafetyCutoff = cutoff,
                BeijingReservationSupported = BeijingReservationCheck.IsChecked == true,
                SoundEnabled = SoundCheck.IsChecked == true,
                FlashTaskbar = FlashCheck.IsChecked == true,
                ToastEnabled = ToastCheck.IsChecked == true,
                DailyHealthSummaryEnabled = HealthSummaryCheck.IsChecked == true,
                AutoStartEnabled = AutoStartCheck.IsChecked == true,
                NormalSyncMinutes = normalMinutes,
                ActiveDaySyncMinutes = activeMinutes,
                OnboardingCompleted = _settings.NotificationSelfTestCompleted,
            };

            await _managementService.SaveSettingsAsync(next);
            var autoStartApplied = await _autoStartService.SetEnabledAsync(next.AutoStartEnabled);
            _settings = next;
            PopulateSettings(_settings);
            SettingsStatusText.Text = autoStartApplied || !next.AutoStartEnabled
                ? "设置已保存，提醒计划已重算"
                : "设置已保存，但自启动计划任务未能更新";
            await RefreshAsync(loadSettings: false);
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "设置无效", MessageBoxButton.OK, MessageBoxImage.Warning);
        }
    }

    private void OpenDataFolder_Click(object sender, RoutedEventArgs e)
    {
        Directory.CreateDirectory(_dataRoot);
        Process.Start(new ProcessStartInfo("explorer.exe", _dataRoot) { UseShellExecute = true });
    }

    private void CopyDiagnostics_Click(object sender, RoutedEventArgs e)
    {
        var snapshot = _runtimeState.Snapshot;
        var builder = new StringBuilder()
            .AppendLine("A 股新股申购提醒 - 诊断摘要")
            .AppendLine(ChineseCulture, $"生成时间：{DateTimeOffset.Now:yyyy-MM-dd HH:mm:ss zzz}")
            .AppendLine(ChineseCulture, $"运行状态：{snapshot.StatusText}")
            .AppendLine(ChineseCulture, $"最近同步：{FormatTimestamp(snapshot.LastSyncCompletedAt)}")
            .AppendLine(ChineseCulture, $"同步成功：{snapshot.LastSyncSucceeded}")
            .AppendLine(ChineseCulture, $"数据目录：{_dataRoot}");
        if (_healthSummary is not null)
        {
            builder.AppendLine(ChineseCulture, $"总体健康：{_healthSummary.OverallState}")
                .AppendLine(ChineseCulture, $"今日任务：{_healthSummary.TodayTaskCount}，待确认：{_healthSummary.PendingConfirmationCount}");
            foreach (var source in _healthSummary.Sources)
            {
                builder.AppendLine(ChineseCulture, $"{source.Source}: {source.State}, 最近成功 {FormatTimestamp(source.LastSuccessAt)}, 连续失败 {source.ConsecutiveFailures}");
            }
        }

        System.Windows.Clipboard.SetText(builder.ToString());
        SettingsStatusText.Text = "诊断摘要已复制到剪贴板";
    }

    private async void ExportDiagnostics_Click(object sender, RoutedEventArgs e)
    {
        var exportButton = sender as WpfButton;
        if (exportButton is not null)
        {
            exportButton.IsEnabled = false;
        }

        try
        {
            SettingsStatusText.Text = "正在生成脱敏诊断包…";
            var path = await _diagnosticBundleService.ExportAsync();
            SettingsStatusText.Text = $"诊断包已导出：{Path.GetFileName(path)}";
            var directory = Path.GetDirectoryName(path);
            if (!string.IsNullOrWhiteSpace(directory))
            {
                Process.Start(new ProcessStartInfo("explorer.exe", directory) { UseShellExecute = true });
            }
        }
        catch (Exception ex)
        {
            SettingsStatusText.Text = "诊断包导出失败";
            MessageBox.Show(ex.Message, "导出诊断包失败", MessageBoxButton.OK, MessageBoxImage.Error);
        }
        finally
        {
            if (exportButton is not null)
            {
                exportButton.IsEnabled = true;
            }
        }
    }

    private static bool TryParseTime(string? value, out TimeOnly result) =>
        TimeOnly.TryParseExact(value?.Trim(), ["H:mm", "HH:mm"], ChineseCulture, DateTimeStyles.None, out result);

    private static bool IsPending(IpoEvent ipoEvent) => ipoEvent.LifecycleStatus is
        IpoLifecycleStatus.Scheduled or IpoLifecycleStatus.ActiveUnconfirmed or IpoLifecycleStatus.AcknowledgedNeedsReview;

    private static string FormatTimestamp(DateTimeOffset? timestamp) =>
        timestamp is null ? "无" : timestamp.Value.ToLocalTime().ToString("yyyy-MM-dd HH:mm:ss", ChineseCulture);

    private static SolidColorBrush Brush(string color) =>
        new((System.Windows.Media.Color)System.Windows.Media.ColorConverter.ConvertFromString(color));

    private sealed class EventCardViewModel
    {
        public EventCardViewModel(IpoEvent ipoEvent, AppSettings settings, DateOnly today)
        {
            Event = ipoEvent;
            Name = ipoEvent.Name;
            var applyCodeText = string.IsNullOrWhiteSpace(ipoEvent.ApplyCode) ? "待核验" : ipoEvent.ApplyCode;
            MarketAndCodes = $"{MarketName(ipoEvent.Exchange, ipoEvent.Board)} · 股票 {ipoEvent.SecurityCode} · 申购 {applyCodeText}";
            var cutoff = ipoEvent.Sessions.Count == 0
                ? settings.SafetyCutoff
                : ipoEvent.Sessions[ipoEvent.Sessions.Count - 1].SafetyCutoff ?? settings.SafetyCutoff;
            DateAndCutoff = $"{ipoEvent.ApplyDate?.ToString("yyyy-MM-dd", ChineseCulture) ?? "待公告确认"} / {cutoff.ToString("HH:mm", ChineseCulture)}";
            var price = ipoEvent.IssuePrice is null ? "价格待公布" : $"{ipoEvent.IssuePrice:0.00} 元";
            var max = ipoEvent.MaxApplyQuantity is null ? "上限待公布" : $"上限 {ipoEvent.MaxApplyQuantity:N0} 股";
            var lot = ipoEvent.LotSize is null ? "单位待公布" : $"单位 {ipoEvent.LotSize:N0} 股";
            NumbersText = $"{price} · {max} · {lot}";
            SessionText = ipoEvent.Sessions.Count == 0
                ? DefaultSessionText(ipoEvent.Exchange)
                : string.Join("；", ipoEvent.Sessions.OrderBy(static session => session.SessionNumber)
                    .Select(session => $"{session.OfficialStart.ToString("HH:mm", ChineseCulture)}–{session.OfficialEnd.ToString("HH:mm", ChineseCulture)}"));
            StatusText = LifecycleName(ipoEvent);
            QualityText = QualityName(ipoEvent.DataQualityStatus);
            UpdatedText = $"最后更新 {ipoEvent.UpdatedAt.ToLocalTime():MM-dd HH:mm}";
            StatusBrush = ipoEvent.LifecycleStatus switch
            {
                IpoLifecycleStatus.Acknowledged => Brush("#166534"),
                IpoLifecycleStatus.ExpiredUnconfirmed => Brush("#991B1B"),
                IpoLifecycleStatus.AcknowledgedNeedsReview => Brush("#9A3412"),
                IpoLifecycleStatus.SuspendedOrCancelled => Brush("#475569"),
                _ => Brush("#1D4ED8"),
            };
            QualityBrush = ipoEvent.DataQualityStatus switch
            {
                DataQualityStatus.AnnouncementVerified or DataQualityStatus.MultiSourceVerified => Brush("#86EFAC"),
                DataQualityStatus.DataConflict or DataQualityStatus.ManualReviewRequired or DataQualityStatus.Stale => Brush("#FCD34D"),
                _ => Brush("#CBD5E1"),
            };
            BorderBrush = ipoEvent.DataQualityStatus is DataQualityStatus.DataConflict or DataQualityStatus.ManualReviewRequired
                || ipoEvent.LifecycleStatus is IpoLifecycleStatus.ExpiredUnconfirmed or IpoLifecycleStatus.AcknowledgedNeedsReview
                ? Brush("#D97706")
                : Brush("#26334A");
            var warnings = new List<string>();
            if (ipoEvent.Exchange == Exchange.Beijing)
            {
                warnings.Add("北交所通常需全额缴付申购资金；不足 100 股余股顺序可能受提交时间影响。" );
            }

            if (ipoEvent.DataQualityStatus is DataQualityStatus.DataConflict or DataQualityStatus.ManualReviewRequired or DataQualityStatus.Stale)
            {
                warnings.Add($"数据状态：{QualityName(ipoEvent.DataQualityStatus)}，请核对正式公告。" );
            }

            if (ipoEvent.ApplyDate is { } applyDate
                && applyDate <= today
                && string.IsNullOrWhiteSpace(ipoEvent.ApplyCode))
            {
                warnings.Add("申购日已到但申购代码仍缺失，请立即人工核验正式公告。" );
            }

            if (ipoEvent.LifecycleStatus == IpoLifecycleStatus.AcknowledgedNeedsReview)
            {
                warnings.Add("关键申购信息已变化，旧确认已失效，请核对后重新确认。" );
            }

            WarningText = string.Join(Environment.NewLine, warnings);
            WarningVisibility = warnings.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
            ConfirmVisibility = IsPending(ipoEvent) ? Visibility.Visible : Visibility.Collapsed;
            RevokeVisibility = ipoEvent.LifecycleStatus == IpoLifecycleStatus.Acknowledged ? Visibility.Visible : Visibility.Collapsed;
            ConfirmButtonText = ipoEvent.LifecycleStatus == IpoLifecycleStatus.AcknowledgedNeedsReview ? "重新确认" : "确认已申购";
        }

        public IpoEvent Event { get; }
        public string Name { get; }
        public string MarketAndCodes { get; }
        public string DateAndCutoff { get; }
        public string NumbersText { get; }
        public string SessionText { get; }
        public string StatusText { get; }
        public string QualityText { get; }
        public string UpdatedText { get; }
        public string WarningText { get; }
        public string ConfirmButtonText { get; }
        public MediaBrush StatusBrush { get; }
        public MediaBrush QualityBrush { get; }
        public MediaBrush BorderBrush { get; }
        public Visibility WarningVisibility { get; }
        public Visibility ConfirmVisibility { get; }
        public Visibility RevokeVisibility { get; }
    }

    private sealed class SourceHealthViewModel
    {
        public SourceHealthViewModel(SourceHealth source)
        {
            Source = source.Source;
            StateText = source.State switch
            {
                HealthState.Healthy => "正常",
                HealthState.Warning => "陈旧/警告",
                HealthState.Failed => "失败",
                _ => "未知",
            };
            StateBrush = source.State switch
            {
                HealthState.Healthy => Brush("#86EFAC"),
                HealthState.Warning => Brush("#FCD34D"),
                HealthState.Failed => Brush("#FCA5A5"),
                _ => Brush("#CBD5E1"),
            };
            BorderBrush = source.State == HealthState.Healthy ? Brush("#26334A") : Brush("#D97706");
            RecordText = $"记录 {source.LastRecordCount}";
            LastSuccessText = $"最近成功 {FormatTimestamp(source.LastSuccessAt)} · 连续失败 {source.ConsecutiveFailures}";
        }

        public string Source { get; }
        public string StateText { get; }
        public string RecordText { get; }
        public string LastSuccessText { get; }
        public MediaBrush StateBrush { get; }
        public MediaBrush BorderBrush { get; }
    }

    private static string MarketName(Exchange exchange, Board board) => (exchange, board) switch
    {
        (Exchange.Shanghai, Board.Star) => "沪市·科创板",
        (Exchange.Shanghai, _) => "沪市·主板",
        (Exchange.Shenzhen, Board.ChiNext) => "深市·创业板",
        (Exchange.Shenzhen, _) => "深市·主板",
        (Exchange.Beijing, _) => "北交所",
        _ => "未知市场",
    };

    private static string DefaultSessionText(Exchange exchange) => exchange == Exchange.Shanghai
        ? "09:30–11:30；13:00–15:00（默认，公告优先）"
        : "09:15–11:30；13:00–15:00（默认，公告优先）";

    private static string LifecycleName(IpoEvent ipoEvent) => ipoEvent.LifecycleStatus switch
    {
        IpoLifecycleStatus.Discovered => "已发现",
        IpoLifecycleStatus.Scheduled => "待申购",
        IpoLifecycleStatus.ActiveUnconfirmed => "待确认",
        IpoLifecycleStatus.Acknowledged => "已确认",
        IpoLifecycleStatus.AcknowledgedNeedsReview => "数据变更·需重确认",
        IpoLifecycleStatus.SuspendedOrCancelled => ipoEvent.Status switch
        {
            IssueStatus.Postponed => "延期发行",
            IssueStatus.Suspended => "暂缓发行",
            IssueStatus.Terminated => "终止发行",
            _ => "已暂停/终止",
        },
        IpoLifecycleStatus.Superseded => "已被新版本替换",
        IpoLifecycleStatus.ExpiredUnconfirmed => "截止仍未确认",
        _ => ipoEvent.LifecycleStatus.ToString(),
    };

    private static string QualityName(DataQualityStatus status) => status switch
    {
        DataQualityStatus.AnnouncementVerified => "正式公告已核验",
        DataQualityStatus.MultiSourceVerified => "多源一致",
        DataQualityStatus.SingleSource => "单一来源待核验",
        DataQualityStatus.DataConflict => "来源冲突",
        DataQualityStatus.Stale => "数据陈旧",
        DataQualityStatus.ManualReviewRequired => "待人工核验",
        _ => status.ToString(),
    };
}
