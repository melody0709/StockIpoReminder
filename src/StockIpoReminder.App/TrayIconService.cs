using System.Drawing;
using System.Windows.Forms;

namespace StockIpoReminder.App;

public sealed class TrayIconService : IDisposable
{
    private readonly NotifyIcon _notifyIcon;
    private readonly MainWindow _mainWindow;

    public TrayIconService(MainWindow mainWindow, Func<Task> exitAsync, Action sync)
    {
        _mainWindow = mainWindow;
        var menu = new ContextMenuStrip();
        menu.Items.Add("今日申购任务", null, (_, _) => Dispatch(mainWindow.ShowAndActivate));
        menu.Items.Add("立即同步", null, (_, _) => sync());
        menu.Items.Add("提醒设置", null, (_, _) => Dispatch(mainWindow.ShowSettings));
        menu.Items.Add(new ToolStripSeparator());
        menu.Items.Add("退出", null, async (_, _) => await mainWindow.Dispatcher.InvokeAsync(exitAsync));

        _notifyIcon = new NotifyIcon
        {
            Icon = SystemIcons.Information,
            Text = "A 股打新提醒 - 正在启动",
            Visible = true,
            ContextMenuStrip = menu,
        };
        _notifyIcon.DoubleClick += (_, _) => Dispatch(mainWindow.ShowAndActivate);
    }

    public void UpdateStatus(int pendingCount, bool unhealthy)
    {
        var state = unhealthy ? "数据异常" : pendingCount > 0 ? $"待确认 {pendingCount} 只" : "运行正常";
        _notifyIcon.Text = ($"A 股打新提醒 - {state}")[..Math.Min(63, $"A 股打新提醒 - {state}".Length)];
    }

    public bool IsVisible => _notifyIcon.Visible;

    public string StatusText => _notifyIcon.Text;

    public void ShowBalloon(string title, string message, ToolTipIcon icon = ToolTipIcon.Info)
    {
        _notifyIcon.BalloonTipTitle = title;
        _notifyIcon.BalloonTipText = message;
        _notifyIcon.BalloonTipIcon = icon;
        _notifyIcon.ShowBalloonTip(8000);
    }

    public void Dispose()
    {
        _notifyIcon.Visible = false;
        _notifyIcon.Dispose();
    }

    private void Dispatch(Action action) => _mainWindow.Dispatcher.BeginInvoke(action);
}
