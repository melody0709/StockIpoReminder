using System.Media;
using System.Windows;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Runtime;
using Application = System.Windows.Application;

namespace StockIpoReminder.App;

public sealed class DesktopReminderSink : IReminderSink
{
    private readonly ReminderManagementService _managementService;
    private readonly ToastNotificationService _toastService;
    private readonly Dictionary<string, ReminderWindow> _windows = new(StringComparer.OrdinalIgnoreCase);
    private TrayIconService? _trayIcon;

    public DesktopReminderSink(ReminderManagementService managementService, ToastNotificationService toastService)
    {
        _managementService = managementService;
        _toastService = toastService;
        _toastService.OpenRequested += (_, eventId) => Application.Current?.Dispatcher.BeginInvoke(() =>
        {
            if (Application.Current.MainWindow is not MainWindow mainWindow)
            {
                return;
            }

            mainWindow.ShowAndActivate();
            if (!string.IsNullOrWhiteSpace(eventId))
            {
                mainWindow.ShowEventDetails(eventId);
            }
        });
    }

    public void AttachTray(TrayIconService trayIcon) => _trayIcon = trayIcon;
    public bool ToastAvailable => _toastService.IsAvailable;
    public bool TrayVisible => _trayIcon?.IsVisible == true;
    public string? TrayStatusText => _trayIcon?.StatusText;

    public void UpdateTrayStatus(int pendingCount, bool unhealthy)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is null)
        {
            return;
        }

        dispatcher.BeginInvoke(() => _trayIcon?.UpdateStatus(pendingCount, unhealthy));
    }

    public async Task ShowAsync(ReminderDelivery reminder, CancellationToken cancellationToken)
    {
        var settings = await _managementService.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
        var dispatcher = Application.Current?.Dispatcher ?? throw new InvalidOperationException("Windows UI 尚未初始化。" );
        await dispatcher.InvokeAsync(() =>
        {
            if (_windows.TryGetValue(reminder.Event.Id, out var existing) && existing.IsLoaded)
            {
                existing.UpdateReminder(reminder, settings);
                existing.ShowWithoutActivation();
                return;
            }

            var window = new ReminderWindow(reminder, settings, _managementService);
            window.Closed += (_, _) => _windows.Remove(reminder.Event.Id);
            _windows[reminder.Event.Id] = window;
            window.ShowWithoutActivation();
        });

        if (settings.ToastEnabled)
        {
            _toastService.ShowReminder(reminder);
        }
    }

    public async Task ShowHealthSummaryAsync(HealthSummary summary, CancellationToken cancellationToken)
    {
        var dispatcher = Application.Current?.Dispatcher ?? throw new InvalidOperationException("Windows UI 尚未初始化。" );
        await dispatcher.InvokeAsync(() =>
        {
            _toastService.ShowHealth(summary);
            var icon = summary.OverallState == HealthState.Healthy
                ? System.Windows.Forms.ToolTipIcon.Info
                : System.Windows.Forms.ToolTipIcon.Warning;
            _trayIcon?.ShowBalloon(
                summary.OverallState == HealthState.Healthy ? "打新提醒运行正常" : "打新提醒需要检查",
                $"今日任务 {summary.TodayTaskCount} 只，待确认 {summary.PendingConfirmationCount} 只。",
                icon);
            if (summary.OverallState == HealthState.Failed)
            {
                new HealthSummaryWindow(summary).ShowWithoutActivation();
            }
        });
    }

    public async Task ShowNotificationTestAsync()
    {
        var settings = await _managementService.GetSettingsAsync().ConfigureAwait(false);
        await Application.Current.Dispatcher.InvokeAsync(() =>
        {
            if (settings.SoundEnabled)
            {
                SystemSounds.Exclamation.Play();
            }

            _toastService.ShowTest();
            _trayIcon?.ShowBalloon("提醒通道测试", "托盘气泡、声音和应用内窗口均已触发。", System.Windows.Forms.ToolTipIcon.Info);
            var test = HealthSummaryWindow.CreateTest();
            test.ShowWithoutActivation();
        });
    }
}
