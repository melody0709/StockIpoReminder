using System.Net.NetworkInformation;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Win32;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class RecoveryEventService : IHostedService, IDisposable
{
    private readonly RecoveryEventCoordinator _coordinator;
    private readonly ILogger<RecoveryEventService> _logger;
    private bool _subscribed;

    public RecoveryEventService(
        RecoveryEventCoordinator coordinator,
        ILogger<RecoveryEventService> logger)
    {
        _coordinator = coordinator;
        _logger = logger;
    }

    public Task StartAsync(CancellationToken cancellationToken)
    {
        try
        {
            SystemEvents.PowerModeChanged += OnPowerModeChanged;
            SystemEvents.SessionSwitch += OnSessionSwitch;
            NetworkChange.NetworkAvailabilityChanged += OnNetworkAvailabilityChanged;
            _subscribed = true;
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "无法注册电源、解锁或网络恢复事件；定时同步仍会继续工作");
        }

        return Task.CompletedTask;
    }

    public Task StopAsync(CancellationToken cancellationToken)
    {
        Unsubscribe();
        return Task.CompletedTask;
    }

    public void Dispose()
    {
        Unsubscribe();
        GC.SuppressFinalize(this);
    }

    private void OnPowerModeChanged(object sender, PowerModeChangedEventArgs args)
    {
        _ = HandlePowerMode(args.Mode);
    }

    private void OnSessionSwitch(object sender, SessionSwitchEventArgs args)
    {
        _ = HandleSessionSwitch(args.Reason);
    }

    private void OnNetworkAvailabilityChanged(object? sender, NetworkAvailabilityEventArgs args)
    {
        _ = HandleNetworkAvailability(args.IsAvailable);
    }

    public RecoveryDispatchResult? HandlePowerMode(PowerModes mode) => mode == PowerModes.Resume
        ? _coordinator.Dispatch(RecoveryEventKind.Resume)
        : null;

    public RecoveryDispatchResult? HandleSessionSwitch(SessionSwitchReason reason) =>
        reason == SessionSwitchReason.SessionUnlock
            ? _coordinator.Dispatch(RecoveryEventKind.SessionUnlock)
            : null;

    public RecoveryDispatchResult? HandleNetworkAvailability(bool isAvailable) => isAvailable
        ? _coordinator.Dispatch(RecoveryEventKind.NetworkAvailable)
        : null;

    private void Unsubscribe()
    {
        if (!_subscribed)
        {
            return;
        }

        SystemEvents.PowerModeChanged -= OnPowerModeChanged;
        SystemEvents.SessionSwitch -= OnSessionSwitch;
        NetworkChange.NetworkAvailabilityChanged -= OnNetworkAvailabilityChanged;
        _subscribed = false;
    }
}
