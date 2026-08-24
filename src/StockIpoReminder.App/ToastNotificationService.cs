using Microsoft.Extensions.Logging;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;
using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.App;

public sealed class ToastNotificationService : IDisposable
{
    private readonly ILogger<ToastNotificationService> _logger;
    private bool _registered;

    public ToastNotificationService(ILogger<ToastNotificationService> logger) => _logger = logger;

    public event EventHandler<string?>? OpenRequested;
    public bool IsAvailable => _registered;

    public void Initialize()
    {
        try
        {
            if (!AppNotificationManager.IsSupported())
            {
                return;
            }

            AppNotificationManager.Default.NotificationInvoked += OnNotificationInvoked;
            AppNotificationManager.Default.Register();
            _registered = true;
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Windows Toast 注册失败，将继续使用应用内提醒窗口");
        }
    }

    public void ShowReminder(ReminderDelivery reminder)
    {
        if (!_registered)
        {
            return;
        }

        try
        {
            var ipoEvent = reminder.Event;
            var notification = new AppNotificationBuilder()
                .AddText($"新股申购待确认：{ipoEvent.Name}")
                .AddText($"申购代码 {ipoEvent.DisplayCode} · {ipoEvent.Exchange} · {ipoEvent.ApplyDate:yyyy-MM-dd}")
                .AddText("只有在程序中确认已申购，后续提醒才会停止。")
                .AddArgument("action", "open")
                .AddArgument("eventId", ipoEvent.Id)
                .AddButton(new AppNotificationButton("打开确认窗口")
                    .AddArgument("action", "open")
                    .AddArgument("eventId", ipoEvent.Id))
                .BuildNotification();
            notification.Tag = SafeTag(ipoEvent.Id);
            notification.Group = "ipo-reminders";
            AppNotificationManager.Default.Show(notification);
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Windows Toast 显示失败");
        }
    }

    public void ShowHealth(HealthSummary summary)
    {
        if (!_registered)
        {
            return;
        }

        try
        {
            var notification = new AppNotificationBuilder()
                .AddText(summary.OverallState == HealthState.Healthy ? "打新提醒运行正常" : "打新提醒需要检查")
                .AddText($"今日任务 {summary.TodayTaskCount} 只，待确认 {summary.PendingConfirmationCount} 只")
                .AddText($"数据源：{summary.Sources.Count(x => x.State == HealthState.Healthy)} 正常 / {summary.Sources.Count(x => x.State != HealthState.Healthy)} 异常")
                .AddArgument("action", "open")
                .BuildNotification();
            notification.Tag = $"health-{summary.GeneratedAt:yyyyMMdd}";
            notification.Group = "health";
            AppNotificationManager.Default.Show(notification);
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "健康摘要 Toast 显示失败");
        }
    }

    public void ShowTest()
    {
        if (!_registered)
        {
            return;
        }

        var notification = new AppNotificationBuilder()
            .AddText("A 股打新提醒测试")
            .AddText("如果你看到了这条通知，Windows Toast 通道工作正常。")
            .BuildNotification();
        AppNotificationManager.Default.Show(notification);
    }

    public void Dispose()
    {
        if (!_registered)
        {
            return;
        }

        try
        {
            AppNotificationManager.Default.NotificationInvoked -= OnNotificationInvoked;
            AppNotificationManager.Default.Unregister();
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "Toast 注销失败");
        }

        _registered = false;
    }

    private void OnNotificationInvoked(AppNotificationManager sender, AppNotificationActivatedEventArgs args)
    {
        args.Arguments.TryGetValue("eventId", out var eventId);
        OpenRequested?.Invoke(this, eventId);
    }

    private static string SafeTag(string value)
    {
        var hash = Core.Services.ValueNormalizer.Sha256(value);
        return hash[..16];
    }
}
