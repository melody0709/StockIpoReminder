using System.Windows;
using System.Windows.Media;
using StockIpoReminder.Core.Domain;
using MediaColor = System.Windows.Media.Color;

namespace StockIpoReminder.App;

public partial class HealthSummaryWindow : Window
{
    public HealthSummaryWindow(HealthSummary summary)
    {
        InitializeComponent();
        TitleText.Text = summary.OverallState == HealthState.Healthy ? "运行正常" : "提醒系统需要检查";
        OuterBorder.BorderBrush = new SolidColorBrush(summary.OverallState switch
        {
            HealthState.Healthy => MediaColor.FromRgb(34, 197, 94),
            HealthState.Warning => MediaColor.FromRgb(245, 158, 11),
            _ => MediaColor.FromRgb(239, 68, 68),
        });
        SummaryText.Text = $"今日任务 {summary.TodayTaskCount} 只，待确认 {summary.PendingConfirmationCount} 只，来源冲突 {summary.ConflictCount} 只，待人工核验 {summary.ManualReviewCount} 只。";
        SourceText.Text = string.Join(Environment.NewLine, summary.Sources.Select(source =>
            $"{source.Source}: {source.State} · 最近成功 {source.LastSuccessAt?.ToLocalTime():MM-dd HH:mm}"));
    }

    private HealthSummaryWindow(string title, string summary)
    {
        InitializeComponent();
        TitleText.Text = title;
        SummaryText.Text = summary;
        SourceText.Text = "关闭此窗口并不会改变任何申购确认状态。";
    }

    public static HealthSummaryWindow CreateTest() => new("应用内提醒窗口测试", "如果你看到了这个置顶窗口，应用内主提醒通道工作正常。");

    public void ShowWithoutActivation() => Show();
    private void Close_Click(object sender, RoutedEventArgs e) => Close();
}
