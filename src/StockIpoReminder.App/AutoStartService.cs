using System.Diagnostics;
using System.IO;
using System.Security;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.App;

public sealed class AutoStartService
{
    private readonly ApplicationRuntimeOptions _runtimeOptions;
    private readonly ILogger<AutoStartService> _logger;

    public AutoStartService(ApplicationRuntimeOptions runtimeOptions, ILogger<AutoStartService> logger)
    {
        _runtimeOptions = runtimeOptions;
        _logger = logger;
    }

    public string TaskName => _runtimeOptions.AutoStartTaskName;

    public async Task<bool> SetEnabledAsync(bool enabled, CancellationToken cancellationToken = default)
    {
        try
        {
            if (!enabled)
            {
                return await RunSchtasksAsync(["/Delete", "/TN", TaskName, "/F"], cancellationToken).ConfigureAwait(false) is 0 or 1;
            }

            var executable = Environment.ProcessPath;
            if (string.IsNullOrWhiteSpace(executable))
            {
                return false;
            }

            var identity = $"{Environment.UserDomainName}\\{Environment.UserName}";
            var xml = $"""
                <?xml version="1.0" encoding="UTF-16"?>
                <Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
                  <RegistrationInfo><Description>A 股新股申购后台提醒</Description></RegistrationInfo>
                  <Triggers>
                    <LogonTrigger><Enabled>true</Enabled><UserId>{SecurityElement.Escape(identity)}</UserId></LogonTrigger>
                  </Triggers>
                  <Principals>
                    <Principal id="Author"><UserId>{SecurityElement.Escape(identity)}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal>
                  </Principals>
                  <Settings>
                    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
                    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
                    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
                    <AllowHardTerminate>true</AllowHardTerminate>
                    <StartWhenAvailable>true</StartWhenAvailable>
                    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
                    <AllowStartOnDemand>true</AllowStartOnDemand>
                    <Enabled>true</Enabled><Hidden>false</Hidden><RunOnlyIfIdle>false</RunOnlyIfIdle><WakeToRun>false</WakeToRun>
                    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Priority>7</Priority>
                    <RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>
                  </Settings>
                  <Actions Context="Author">
                    <Exec><Command>{SecurityElement.Escape(executable)}</Command><Arguments>{SecurityElement.Escape(_runtimeOptions.AutoStartArguments)}</Arguments><WorkingDirectory>{SecurityElement.Escape(AppContext.BaseDirectory)}</WorkingDirectory></Exec>
                  </Actions>
                </Task>
                """;
            var path = Path.Combine(Path.GetTempPath(), $"stock-ipo-reminder-{Guid.NewGuid():N}.xml");
            try
            {
                await File.WriteAllTextAsync(path, xml, System.Text.Encoding.Unicode, cancellationToken).ConfigureAwait(false);
                return await RunSchtasksAsync(["/Create", "/TN", TaskName, "/XML", path, "/F"], cancellationToken).ConfigureAwait(false) == 0;
            }
            finally
            {
                File.Delete(path);
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            _logger.LogWarning(ex, "无法更新登录自启动计划任务");
            return false;
        }
    }

    private static async Task<int> RunSchtasksAsync(IReadOnlyList<string> arguments, CancellationToken cancellationToken)
    {
        var startInfo = new ProcessStartInfo("schtasks.exe")
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        using var process = Process.Start(startInfo) ?? throw new InvalidOperationException("无法启动 schtasks.exe。" );
        await process.WaitForExitAsync(cancellationToken).ConfigureAwait(false);
        return process.ExitCode;
    }
}
