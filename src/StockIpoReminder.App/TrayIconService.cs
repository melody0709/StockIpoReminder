using System.Drawing;
using System.Windows.Forms;

namespace StockIpoReminder.App;

public sealed class TrayIconService : IDisposable
{
    private readonly ContextMenuStrip _contextMenu;
    private readonly NotifyIcon _notifyIcon;
    private readonly System.Drawing.Icon _applicationIcon;
    private readonly MainWindow _mainWindow;

    public TrayIconService(MainWindow mainWindow, Func<Task> exitAsync, Action sync)
    {
        _mainWindow = mainWindow;
        _contextMenu = new ContextMenuStrip();
        _contextMenu.Items.Add(new ToolStripLabel($"A 股打新提醒 v{GetApplicationVersion()}")
        {
            Margin = new Padding(8, 4, 8, 4),
        });
        _contextMenu.Items.Add(new ToolStripSeparator());
        _contextMenu.Items.Add("今日申购任务", null, (_, _) => Dispatch(mainWindow.ShowAndActivate));
        _contextMenu.Items.Add("立即同步", null, (_, _) => sync());
        _contextMenu.Items.Add("提醒设置", null, (_, _) => Dispatch(mainWindow.ShowSettings));
        _contextMenu.Items.Add(new ToolStripSeparator());
        _contextMenu.Items.Add("退出", null, async (_, _) => await mainWindow.Dispatcher.InvokeAsync(exitAsync));

        _applicationIcon = LoadApplicationIcon();
        _notifyIcon = new NotifyIcon
        {
            Icon = _applicationIcon,
            Text = "A 股打新提醒 - 正在启动",
            Visible = true,
        };
        _notifyIcon.MouseUp += NotifyIcon_MouseUp;
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
        _contextMenu.Dispose();
        _applicationIcon.Dispose();
    }

    private void Dispatch(Action action) => _mainWindow.Dispatcher.BeginInvoke(action);

    private void NotifyIcon_MouseUp(object? sender, MouseEventArgs eventArgs)
    {
        if (eventArgs.Button == MouseButtons.Right)
        {
            ShowContextMenuAboveTaskbar();
        }
    }

    private void ShowContextMenuAboveTaskbar()
    {
        var cursorPosition = Cursor.Position;
        var workingArea = Screen.FromPoint(cursorPosition).WorkingArea;
        var menuSize = _contextMenu.GetPreferredSize(System.Drawing.Size.Empty);
        var maxX = Math.Max(workingArea.Left, workingArea.Right - menuSize.Width);
        var x = Math.Clamp(cursorPosition.X - menuSize.Width + 12, workingArea.Left, maxX);
        var y = Math.Max(workingArea.Top, workingArea.Bottom - menuSize.Height);
        _contextMenu.Show(new System.Drawing.Point(x, y));
    }

    private static string GetApplicationVersion() =>
        typeof(TrayIconService).Assembly.GetName().Version?.ToString(3) ?? "未知版本";

    private static System.Drawing.Icon LoadApplicationIcon()
    {
        var executablePath = Environment.ProcessPath;
        if (!string.IsNullOrWhiteSpace(executablePath) && System.IO.File.Exists(executablePath))
        {
            try
            {
                return System.Drawing.Icon.ExtractAssociatedIcon(executablePath)
                    ?? (System.Drawing.Icon)System.Drawing.SystemIcons.Information.Clone();
            }
            catch (ArgumentException)
            {
                // 测试宿主或非常规启动环境可能没有可提取的关联图标。
            }
        }

        return (System.Drawing.Icon)System.Drawing.SystemIcons.Information.Clone();
    }
}
