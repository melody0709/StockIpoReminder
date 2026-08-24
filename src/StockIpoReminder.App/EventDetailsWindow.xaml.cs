using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Runtime;
using MessageBox = System.Windows.MessageBox;
using WpfButton = System.Windows.Controls.Button;

namespace StockIpoReminder.App;

public partial class EventDetailsWindow : Window
{
    private static readonly CultureInfo ChineseCulture = CultureInfo.GetCultureInfo("zh-CN");
    private readonly string _eventId;
    private readonly ReminderManagementService _managementService;
    private IpoEventDetails? _details;

    public EventDetailsWindow(string eventId, ReminderManagementService managementService)
    {
        InitializeComponent();
        _eventId = eventId;
        _managementService = managementService;
        OverrideFieldCombo.ItemsSource = new[]
        {
            new FieldChoice("申购代码", "ApplyCode", "例如 787001"),
            new FieldChoice("申购日期", "ApplyDate", "例如 2026-08-26"),
            new FieldChoice("发行价格", "IssuePrice", "例如 12.34"),
            new FieldChoice("申购上限（股）", "MaxApplyQuantity", "例如 15000"),
            new FieldChoice("申购单位（股）", "LotSize", "例如 500"),
            new FieldChoice("官方申购时段", "OfficialSessions", "例如 09:30-11:30,13:00-15:00"),
            new FieldChoice("发行状态", "IssueStatus", "正常发行/暂缓发行/中止发行/终止发行/发行完成"),
        };
        OverrideFieldCombo.DisplayMemberPath = nameof(FieldChoice.DisplayName);
        OverrideFieldCombo.SelectedIndex = 0;
        OverrideFieldCombo.SelectionChanged += (_, _) =>
        {
            if (OverrideFieldCombo.SelectedItem is FieldChoice choice)
            {
                OverrideValueText.ToolTip = choice.Example;
            }
        };
    }

    public event EventHandler? EventChanged;

    internal Task RefreshForSmokeAsync() => RefreshAsync();

    private async void Window_Loaded(object sender, RoutedEventArgs e) => await RefreshAsync();

    private async Task RefreshAsync()
    {
        try
        {
            _details = await _managementService.GetEventDetailsAsync(_eventId);
            var ipoEvent = _details.Event;
            TitleText.Text = $"{ipoEvent.Name} · {ipoEvent.DisplayCode}";
            SummaryText.Text = $"{MarketName(ipoEvent.Exchange)} · 股票代码 {ipoEvent.SecurityCode} · 申购日 {ipoEvent.ApplyDate?.ToString("yyyy-MM-dd", ChineseCulture) ?? "待核验"} · 数据版本 {ipoEvent.EventVersion}";
            QualityText.Text = QualityName(ipoEvent.DataQualityStatus);
            QualityBadge.Background = ipoEvent.DataQualityStatus is DataQualityStatus.DataConflict or DataQualityStatus.ManualReviewRequired or DataQualityStatus.Stale
                ? Brush("#9A3412")
                : Brush("#166534");
            WarningText.Text = ipoEvent.HasManualOverride
                ? $"当前有效人工覆盖：{string.Join("、", ipoEvent.ManualOverrideFields.Select(FieldDisplayName))}。所有原始来源仍保留在“字段来源”中。"
                : ipoEvent.DataConflict
                    ? "关键字段存在来源冲突，请以最新正式发行公告为准。"
                    : string.Empty;

            var sourceRows = _details.FieldSources.Select(source => new FieldSourceRow(source)).ToArray();
            FieldSourceList.ItemsSource = sourceRows;
            FieldSourceEmptyText.Visibility = sourceRows.Length == 0 ? Visibility.Visible : Visibility.Collapsed;

            var announcementRows = _details.Announcements.Select(document => new AnnouncementRow(document)).ToArray();
            AnnouncementItems.ItemsSource = announcementRows;
            AnnouncementEmptyText.Visibility = announcementRows.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
            var announcementChoices = new List<AnnouncementChoice> { new(null, "不指定公告") };
            announcementChoices.AddRange(_details.Announcements.Select(document => new AnnouncementChoice(document.Id, document.Reference.Title)));
            OverrideAnnouncementCombo.ItemsSource = announcementChoices;
            OverrideAnnouncementCombo.SelectedIndex = 0;

            var announcementTitles = _details.Announcements.ToDictionary(
                static document => document.Id,
                static document => document.Reference.Title,
                StringComparer.OrdinalIgnoreCase);
            OverrideItems.ItemsSource = _details.ManualOverrides.Select(entry => new OverrideRow(
                entry,
                entry.AnnouncementDocumentId is not null
                    && announcementTitles.TryGetValue(entry.AnnouncementDocumentId, out var title)
                    ? title
                    : null)).ToArray();
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "读取详情失败", MessageBoxButton.OK, MessageBoxImage.Error);
            Close();
        }
    }

    private async void SaveOverride_Click(object sender, RoutedEventArgs e)
    {
        if (_details is null || OverrideFieldCombo.SelectedItem is not FieldChoice field)
        {
            return;
        }

        var announcementId = (OverrideAnnouncementCombo.SelectedItem as AnnouncementChoice)?.Id;
        if (MessageBox.Show(
                $"确定以人工核验值覆盖“{field.DisplayName}”吗？\n\n覆盖值：{OverrideValueText.Text}\n此操作会记录审计信息，并重算后续提醒。",
                "确认人工覆盖",
                MessageBoxButton.YesNo,
                MessageBoxImage.Warning,
                MessageBoxResult.No) != MessageBoxResult.Yes)
        {
            return;
        }

        try
        {
            await _managementService.ApplyManualOverrideAsync(
                _details.Event.Id,
                _details.Event.EventVersion,
                field.FieldName,
                OverrideValueText.Text,
                OverrideReasonText.Text,
                announcementId);
            OverrideValueText.Clear();
            OverrideReasonText.Clear();
            OverrideStatusText.Text = "人工覆盖已保存，提醒计划已重算";
            EventChanged?.Invoke(this, EventArgs.Empty);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "保存人工覆盖失败", MessageBoxButton.OK, MessageBoxImage.Warning);
        }
    }

    private async void RevokeOverride_Click(object sender, RoutedEventArgs e)
    {
        if (_details is null || sender is not WpfButton { Tag: ManualOverrideEntry entry })
        {
            return;
        }

        if (MessageBox.Show(
                $"确定撤销字段“{FieldDisplayName(entry.FieldName)}”的人工覆盖吗？撤销后将恢复公开来源值并重算提醒。",
                "撤销人工覆盖",
                MessageBoxButton.YesNo,
                MessageBoxImage.Warning,
                MessageBoxResult.No) != MessageBoxResult.Yes)
        {
            return;
        }

        try
        {
            await _managementService.RevokeManualOverrideAsync(
                _details.Event.Id,
                _details.Event.EventVersion,
                entry.Id);
            OverrideStatusText.Text = "人工覆盖已撤销，提醒计划已重算";
            EventChanged?.Invoke(this, EventArgs.Empty);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "撤销人工覆盖失败", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private void OpenAnnouncement_Click(object sender, RoutedEventArgs e)
    {
        if (sender is WpfButton { Tag: AnnouncementDocument document })
        {
            Process.Start(new ProcessStartInfo(document.Reference.Url.ToString()) { UseShellExecute = true });
        }
    }

    private void OpenLocalAnnouncement_Click(object sender, RoutedEventArgs e)
    {
        if (sender is WpfButton { Tag: AnnouncementDocument document } && File.Exists(document.LocalPath))
        {
            Process.Start(new ProcessStartInfo(document.LocalPath) { UseShellExecute = true });
        }
    }

    private sealed record FieldChoice(string DisplayName, string FieldName, string Example);
    private sealed record AnnouncementChoice(string? Id, string Title);

    private sealed class FieldSourceRow
    {
        public FieldSourceRow(SourceFieldValue source)
        {
            FieldName = FieldDisplayName(source.FieldName);
            NormalizedValue = source.NormalizedValue ?? "—";
            RawValue = source.RawValue ?? "—";
            Source = source.Source;
            Priority = source.Priority;
            FetchedText = source.FetchedAt.ToLocalTime().ToString("yyyy-MM-dd HH:mm", ChineseCulture);
        }

        public string FieldName { get; }
        public string NormalizedValue { get; }
        public string RawValue { get; }
        public string Source { get; }
        public int Priority { get; }
        public string FetchedText { get; }
    }

    private sealed class AnnouncementRow
    {
        public AnnouncementRow(AnnouncementDocument document)
        {
            Document = document;
            Title = document.Reference.Title;
            Metadata = $"{document.Reference.Provider} · {document.Reference.PublishedAt?.ToLocalTime():yyyy-MM-dd HH:mm} · {ExtractionName(document.ExtractionStatus)} · SHA-256 {document.FileHash[..Math.Min(12, document.FileHash.Length)]}…";
            Evidence = document.ParsedFields.Count == 0
                ? "未提取到高置信度字段，请人工查看原文。"
                : string.Join("；", document.ParsedFields.Take(6).Select(field => $"{FieldDisplayName(field.Name)}={field.Value}（{field.Confidence:P0}）"));
        }

        public AnnouncementDocument Document { get; }
        public string Title { get; }
        public string Metadata { get; }
        public string Evidence { get; }
    }

    private sealed class OverrideRow
    {
        public OverrideRow(ManualOverrideEntry entry, string? announcementTitle)
        {
            Entry = entry;
            Summary = $"{FieldDisplayName(entry.FieldName)} = {entry.OverrideValue}";
            Metadata = $"理由：{entry.Reason} · {entry.CreatedAt.ToLocalTime():yyyy-MM-dd HH:mm}" +
                (entry.AnnouncementDocumentId is null
                    ? " · 未关联依据公告"
                    : $" · 依据公告：{announcementTitle ?? entry.AnnouncementDocumentId}") +
                (entry.RevokedAt is null ? " · 当前有效" : $" · 已于 {entry.RevokedAt.Value.ToLocalTime():yyyy-MM-dd HH:mm} 撤销");
            RevokeVisibility = entry.RevokedAt is null ? Visibility.Visible : Visibility.Collapsed;
        }

        public ManualOverrideEntry Entry { get; }
        public string Summary { get; }
        public string Metadata { get; }
        public Visibility RevokeVisibility { get; }
    }

    private static string FieldDisplayName(string name) => name switch
    {
        "SecurityCode" => "股票代码",
        "ApplyCode" => "申购代码",
        "Name" => "股票简称",
        "ApplyDate" => "申购日期",
        "IssuePrice" => "发行价格",
        "LotSize" => "申购单位",
        "MaxApplyQuantity" => "申购上限",
        "IssueStatus" or "Status" => "发行状态",
        "OfficialSessions" or "Sessions" => "官方申购时段",
        _ => name,
    };

    private static string MarketName(Exchange exchange) => exchange switch
    {
        Exchange.Shanghai => "沪市",
        Exchange.Shenzhen => "深市",
        Exchange.Beijing => "北交所",
        _ => "未知市场",
    };

    private static string QualityName(DataQualityStatus status) => status switch
    {
        DataQualityStatus.AnnouncementVerified => "公告已核验",
        DataQualityStatus.MultiSourceVerified => "多源一致",
        DataQualityStatus.SingleSource => "单一来源",
        DataQualityStatus.DataConflict => "来源冲突",
        DataQualityStatus.Stale => "数据陈旧",
        DataQualityStatus.ManualReviewRequired => "待人工核验",
        _ => status.ToString(),
    };

    private static string ExtractionName(ExtractionStatus status) => status switch
    {
        ExtractionStatus.Extracted => "文本已解析",
        ExtractionStatus.LowConfidence => "低置信度",
        ExtractionStatus.Failed => "解析失败",
        ExtractionStatus.Unsupported => "不支持自动解析",
        _ => "待解析",
    };

    private static SolidColorBrush Brush(string color) =>
        new((System.Windows.Media.Color)System.Windows.Media.ColorConverter.ConvertFromString(color));
}
