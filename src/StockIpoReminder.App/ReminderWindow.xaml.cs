using System.Diagnostics;
using System.Globalization;
using System.Media;
using System.Windows;
using System.Windows.Media;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Runtime;
using Application = System.Windows.Application;
using MessageBox = System.Windows.MessageBox;

namespace StockIpoReminder.App;

public partial class ReminderWindow : Window
{
    private static readonly CultureInfo ChineseCulture = CultureInfo.GetCultureInfo("zh-CN");
    private ReminderDelivery _reminder;
    private AppSettings _settings;
    private readonly ReminderManagementService _managementService;

    public ReminderWindow(ReminderDelivery reminder, AppSettings settings, ReminderManagementService managementService)
    {
        InitializeComponent();
        _reminder = reminder;
        _settings = settings;
        _managementService = managementService;
        UpdateContent();
        SourceInitialized += (_, _) => PositionWindow();
    }

    public void UpdateReminder(ReminderDelivery reminder, AppSettings settings)
    {
        _reminder = reminder;
        _settings = settings;
        UpdateContent();
        SignalAttention();
    }

    public void ShowWithoutActivation()
    {
        if (!IsVisible)
        {
            Show();
        }

        PositionWindow();
        SignalAttention();
    }

    private void UpdateContent()
    {
        var ipoEvent = _reminder.Event;
        NameText.Text = ipoEvent.Name;
        var applyCodeText = string.IsNullOrWhiteSpace(ipoEvent.ApplyCode) ? "待核验" : ipoEvent.ApplyCode;
        CodeText.Text = $"{MarketName(ipoEvent.Exchange)} · 股票 {ipoEvent.SecurityCode} · 申购 {applyCodeText}";
        DateText.Text = ipoEvent.ApplyDate?.ToString("yyyy-MM-dd", ChineseCulture) ?? "待公告确认";
        PriceText.Text = ipoEvent.IssuePrice is null ? "待公布" : $"{ipoEvent.IssuePrice:0.00} 元";
        QuantityText.Text = $"{(ipoEvent.MaxApplyQuantity is null ? "待公布" : $"{ipoEvent.MaxApplyQuantity:N0} 股")} / {(ipoEvent.LotSize is null ? "待公布" : $"{ipoEvent.LotSize} 股")}";
        var cutoff = ipoEvent.Sessions.Count == 0
            ? _settings.SafetyCutoff
            : ipoEvent.Sessions[ipoEvent.Sessions.Count - 1].SafetyCutoff ?? _settings.SafetyCutoff;
        CutoffText.Text = cutoff.ToString("HH:mm", ChineseCulture);
        LevelText.Text = LevelName(_reminder.Level);
        LevelBadge.Background = _reminder.Level >= ReminderLevel.FiveMinutes
            ? new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(185, 28, 28))
            : new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(29, 78, 216));

        var warnings = new List<string>();
        if (ipoEvent.Exchange == Exchange.Beijing)
        {
            warnings.Add("北交所申购通常需要全额缴付申购资金，并建议尽早提交。" );
        }

        if (ipoEvent.DataQualityStatus is DataQualityStatus.DataConflict or DataQualityStatus.ManualReviewRequired or DataQualityStatus.Stale)
        {
            warnings.Add($"数据状态：{QualityName(ipoEvent.DataQualityStatus)}，请打开正式公告核对。" );
        }

        if (string.IsNullOrWhiteSpace(ipoEvent.ApplyCode))
        {
            warnings.Add("申购代码仍缺失，请先人工核验正式公告，不要把股票代码误当作申购代码。" );
        }

        if (ipoEvent.LifecycleStatus == IpoLifecycleStatus.AcknowledgedNeedsReview)
        {
            warnings.Add("关键申购信息已变化，旧确认已失效，需要重新确认。" );
        }

        WarningPanel.Visibility = warnings.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        WarningText.Text = string.Join(Environment.NewLine, warnings);
    }

    private async void Confirm_Click(object sender, RoutedEventArgs e)
    {
        var answer = MessageBox.Show(
            $"请确认你已经在券商客户端提交了 {_reminder.Event.Name}（申购代码 {_reminder.Event.DisplayCode}）的申购委托。\n\n本程序不会检查委托是否受理或成功。确认后，本申购日内将停止该股票的重复提醒。",
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
            await _managementService.AcknowledgeAsync(_reminder.Event.Id, _reminder.Event.EventVersion);
            Close();
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "确认失败", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private void Later_Click(object sender, RoutedEventArgs e) => Close();

    private void OpenDetails_Click(object sender, RoutedEventArgs e)
    {
        (Application.Current.MainWindow as MainWindow)?.ShowEventDetails(_reminder.Event.Id);
    }

    private void SignalAttention()
    {
        if (_settings.SoundEnabled)
        {
            SystemSounds.Exclamation.Play();
        }

        if (_settings.FlashTaskbar && _reminder.Level >= ReminderLevel.FiveMinutes)
        {
            AttentionService.Flash(this);
        }
    }

    private void PositionWindow()
    {
        var screen = System.Windows.Forms.Screen.FromPoint(System.Windows.Forms.Cursor.Position);
        var dpi = VisualTreeHelper.GetDpi(this);
        Left = screen.WorkingArea.Right / dpi.DpiScaleX - ActualWidth - 18;
        Top = screen.WorkingArea.Bottom / dpi.DpiScaleY - ActualHeight - 18;
    }

    private static string MarketName(Exchange exchange) => exchange switch
    {
        Exchange.Shanghai => "沪市",
        Exchange.Shenzhen => "深市",
        Exchange.Beijing => "北交所",
        _ => "未知市场",
    };

    private static string LevelName(ReminderLevel level) => level switch
    {
        ReminderLevel.Advance => "明日预告",
        ReminderLevel.Morning => "今日申购",
        ReminderLevel.BrokerOpening => "券商已受理",
        ReminderLevel.MarketOpening => "即将开盘",
        ReminderLevel.Hourly => "待确认",
        ReminderLevel.NoonBoundary => "上午将结束",
        ReminderLevel.AfternoonOpening => "下午将开始",
        ReminderLevel.FifteenMinutes => "临近截止",
        ReminderLevel.FiveMinutes => "高频提醒",
        ReminderLevel.TwoMinutes => "紧急提醒",
        ReminderLevel.Final => "最后提醒",
        ReminderLevel.DataChanged => "数据有变更",
        _ => "申购提醒",
    };

    private static string QualityName(DataQualityStatus status) => status switch
    {
        DataQualityStatus.DataConflict => "来源冲突",
        DataQualityStatus.ManualReviewRequired => "待人工核验",
        DataQualityStatus.Stale => "数据陈旧",
        DataQualityStatus.SingleSource => "单一来源",
        DataQualityStatus.MultiSourceVerified => "多源一致",
        DataQualityStatus.AnnouncementVerified => "公告已核验",
        _ => status.ToString(),
    };
}
