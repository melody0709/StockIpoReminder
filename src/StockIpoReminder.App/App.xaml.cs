using System.Diagnostics.CodeAnalysis;
using System.IO;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using System.Windows;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure;
using StockIpoReminder.Infrastructure.Runtime;
using MessageBox = System.Windows.MessageBox;

namespace StockIpoReminder.App;

[SuppressMessage(
    "Design",
    "CA1001:Types that own disposable fields should be disposable",
    Justification = "WPF owns the Application lifetime; OnExit stops and disposes the host, tray icon, toast service, and mutex.")]
public partial class App : System.Windows.Application
{
    private static readonly JsonSerializerOptions StartupProbeJsonOptions = new() { WriteIndented = true };
    private Mutex? _mutex;
    private IHost? _host;
    private TrayIconService? _trayIcon;
    private ToastNotificationService? _toastService;
    private ApplicationRuntimeOptions? _runtimeOptions;
    private bool _isExiting;

    public static IServiceProvider Services => ((App)Current)._host?.Services
        ?? throw new InvalidOperationException("应用服务尚未初始化。");

    public bool IsExiting => _isExiting;

    protected override async void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        try
        {
            _runtimeOptions = ApplicationRuntimeOptions.Parse(e.Args);
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            MessageBox.Show($"启动参数无效：{ex.Message}", "A 股打新提醒", MessageBoxButton.OK, MessageBoxImage.Error);
            Shutdown(2);
            return;
        }

        _mutex = new Mutex(initiallyOwned: true, _runtimeOptions.MutexName, out var createdNew);
        if (!createdNew)
        {
            if (!_runtimeOptions.Background && _runtimeOptions.ReadyFile is null)
            {
                MessageBox.Show("A 股打新提醒已经在后台运行，请查看系统托盘。", "A 股打新提醒", MessageBoxButton.OK, MessageBoxImage.Information);
            }

            await WriteStartupProbeAsync(_runtimeOptions, "already-running", toastAvailable: false, mainWindowVisible: false);
            Shutdown(3);
            return;
        }

        var dataRoot = _runtimeOptions.DataRoot;
        Directory.CreateDirectory(dataRoot);
        var builder = Host.CreateApplicationBuilder();
        builder.Logging.ClearProviders();
        builder.Logging.AddProvider(new FileLoggerProvider(Path.Combine(dataRoot, "logs")));
        builder.Services.AddSingleton(_runtimeOptions);
        builder.Services.AddStockIpoReminderInfrastructure(dataRoot, enableHostedServices: !_runtimeOptions.SmokeMode);
        builder.Services.AddSingleton<ToastNotificationService>();
        builder.Services.AddSingleton<DesktopReminderSink>();
        builder.Services.AddSingleton<IReminderSink>(provider => provider.GetRequiredService<DesktopReminderSink>());
        builder.Services.AddSingleton<AutoStartService>();
        builder.Services.AddSingleton<MainWindow>();
        builder.Services.AddSingleton<UiSmokeScenarioSeeder>();
        builder.Services.AddSingleton<UiSmokeRunner>();
        builder.Services.AddSingleton<ProcessSmokeRunner>();
        builder.Services.AddSingleton<RecoverySmokeRunner>();
        builder.Services.AddSingleton<RecoveryEventService>();
        builder.Services.AddSingleton<IHostedService>(provider => provider.GetRequiredService<RecoveryEventService>());
        builder.Services.AddSingleton<TrayStatusHostedService>();
        builder.Services.AddSingleton<IHostedService>(provider => provider.GetRequiredService<TrayStatusHostedService>());
        _host = builder.Build();

        DispatcherUnhandledException += (_, args) =>
        {
            _host.Services.GetRequiredService<ILogger<App>>().LogError(args.Exception, "未处理的 UI 异常");
            MessageBox.Show(args.Exception.Message, "程序发生错误", MessageBoxButton.OK, MessageBoxImage.Error);
            args.Handled = true;
        };
        AppDomain.CurrentDomain.UnhandledException += (_, args) =>
            _host.Services.GetRequiredService<ILogger<App>>().LogCritical(args.ExceptionObject as Exception, "未处理的进程异常");

        try
        {
            await _host.Services.GetRequiredService<IIpoRepository>().InitializeAsync();
            if (_runtimeOptions.SmokeSeedScenarios)
            {
                await _host.Services.GetRequiredService<UiSmokeScenarioSeeder>().SeedAsync();
            }

            _toastService = _host.Services.GetRequiredService<ToastNotificationService>();
            if (!_runtimeOptions.SmokeMode)
            {
                _toastService.Initialize();
            }
            await _host.StartAsync();

            var autoStartConfigured = false;
            var autoStartService = _host.Services.GetRequiredService<AutoStartService>();
            var settings = await _host.Services.GetRequiredService<ReminderManagementService>().GetSettingsAsync();
            if (_runtimeOptions.SmokeEnableAutoStart)
            {
                autoStartConfigured = await autoStartService.SetEnabledAsync(true);
                if (!autoStartConfigured)
                {
                    throw new InvalidOperationException("smoke 模式无法注册登录自启动计划任务。");
                }
            }
            else if (settings.OnboardingCompleted && settings.AutoStartEnabled)
            {
                autoStartConfigured = await autoStartService.SetEnabledAsync(true);
            }

            var mainWindow = _host.Services.GetRequiredService<MainWindow>();
            MainWindow = mainWindow;
            _trayIcon = new TrayIconService(
                mainWindow,
                RequestExitAsync,
                () => _host.Services.GetRequiredService<ReminderManagementService>().RequestSync());
            _host.Services.GetRequiredService<DesktopReminderSink>().AttachTray(_trayIcon);

            if (!_runtimeOptions.Background || _runtimeOptions.UiSmokeReport is not null)
            {
                mainWindow.ShowAndActivate();
            }

            await WriteStartupProbeAsync(
                _runtimeOptions,
                "ready",
                _toastService.IsAvailable,
                mainWindow.IsVisible,
                autoStartConfigured: autoStartConfigured,
                trayIconVisible: _trayIcon.IsVisible,
                trayStatusText: _trayIcon.StatusText);
            if (_runtimeOptions.ProcessSmokeReport is not null)
            {
                var succeeded = await _host.Services.GetRequiredService<ProcessSmokeRunner>().RunAsync();
                if (!succeeded || _runtimeOptions.ProcessSmokePhase == ProcessSmokeStage.Verify)
                {
                    _isExiting = true;
                    Shutdown(succeeded ? 0 : 5);
                }

                return;
            }

            if (_runtimeOptions.RecoverySmokeReport is not null)
            {
                var succeeded = await _host.Services.GetRequiredService<RecoverySmokeRunner>().RunAsync();
                _isExiting = true;
                Shutdown(succeeded ? 0 : 6);
                return;
            }

            if (_runtimeOptions.UiSmokeReport is not null)
            {
                var succeeded = await _host.Services.GetRequiredService<UiSmokeRunner>().RunAsync(mainWindow);
                _isExiting = true;
                Shutdown(succeeded ? 0 : 4);
                return;
            }

            if (_runtimeOptions.ExitAfter is { } exitAfter)
            {
                _ = ExitAfterDelayAsync(exitAfter);
            }
        }
        catch (Exception ex)
        {
            await WriteStartupProbeAsync(
                _runtimeOptions,
                "failed",
                toastAvailable: false,
                mainWindowVisible: false,
                exception: ex);
            MessageBox.Show($"程序启动失败：{ex.Message}", "A 股打新提醒", MessageBoxButton.OK, MessageBoxImage.Error);
            _isExiting = true;
            Shutdown(1);
        }
    }

    public async Task RequestExitAsync()
    {
        if (_host is null || _isExiting)
        {
            return;
        }

        var management = _host.Services.GetRequiredService<ReminderManagementService>();
        var today = ChinaTime.Today(TimeProvider.System);
        var pending = (await management.GetEventsAsync(today, today))
            .Count(static item => item.LifecycleStatus is IpoLifecycleStatus.Scheduled
                or IpoLifecycleStatus.ActiveUnconfirmed
                or IpoLifecycleStatus.AcknowledgedNeedsReview);
        if (pending > 0)
        {
            var answer = MessageBox.Show(
                $"今天仍有 {pending} 只新股没有确认申购。退出后程序无法继续提醒，确定退出吗？",
                "仍有未确认任务",
                MessageBoxButton.YesNo,
                MessageBoxImage.Warning,
                MessageBoxResult.No);
            if (answer != MessageBoxResult.Yes)
            {
                return;
            }
        }

        _isExiting = true;
        Shutdown();
    }

    protected override async void OnExit(ExitEventArgs e)
    {
        _isExiting = true;
        _trayIcon?.Dispose();
        _toastService?.Dispose();
        if (_host is not null)
        {
            try
            {
                await _host.StopAsync(TimeSpan.FromSeconds(5));
            }
            finally
            {
                _host.Dispose();
            }
        }

        try
        {
            _mutex?.ReleaseMutex();
        }
        catch (ApplicationException)
        {
        }

        _mutex?.Dispose();
        base.OnExit(e);
    }

    private async Task ExitAfterDelayAsync(TimeSpan delay)
    {
        await Task.Delay(delay).ConfigureAwait(false);
        await Dispatcher.InvokeAsync(() =>
        {
            _isExiting = true;
            Shutdown();
        });
    }

    private static async Task WriteStartupProbeAsync(
        ApplicationRuntimeOptions options,
        string status,
        bool toastAvailable,
        bool mainWindowVisible,
        bool autoStartConfigured = false,
        bool trayIconVisible = false,
        string? trayStatusText = null,
        Exception? exception = null)
    {
        if (options.ReadyFile is null)
        {
            return;
        }

        var directory = Path.GetDirectoryName(options.ReadyFile);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        var payload = JsonSerializer.Serialize(new
        {
            status,
            version = Assembly.GetEntryAssembly()?.GetName().Version?.ToString(3) ?? "unknown",
            processId = Environment.ProcessId,
            dataRoot = options.DataRoot,
            options.InstanceKey,
            options.Background,
            options.SmokeMode,
            options.SmokeEnableAutoStart,
            options.SmokeSeedScenarios,
            options.UiSmokeReport,
            processSmokeStage = options.ProcessSmokePhase?.ToString(),
            options.ProcessSmokeReport,
            toastAvailable,
            mainWindowVisible,
            autoStartConfigured,
            trayIconVisible,
            trayStatusText,
            timestampUtc = DateTimeOffset.UtcNow,
            errorType = exception?.GetType().Name,
            error = exception?.Message,
        }, StartupProbeJsonOptions);
        var temporaryPath = options.ReadyFile + $".{Guid.NewGuid():N}.tmp";
        await File.WriteAllTextAsync(temporaryPath, payload).ConfigureAwait(false);
        File.Move(temporaryPath, options.ReadyFile, overwrite: true);
    }
}
