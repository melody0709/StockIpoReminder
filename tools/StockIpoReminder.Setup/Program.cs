using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Windows.Forms;

namespace StockIpoReminder.Setup;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        ApplicationConfiguration.Initialize();
        SetupOptions options;
        try
        {
            options = SetupOptions.Parse(args);
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            MessageBox.Show(ex.Message, ProductConstants.DisplayName, MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 2;
        }

        var executableName = Path.GetFileNameWithoutExtension(Environment.ProcessPath ?? string.Empty);
        var uninstall = options.Uninstall || executableName.Contains("Uninstaller", StringComparison.OrdinalIgnoreCase);
        if (uninstall && !options.FromTemp && TryGetAdjacentManifest(out var adjacentManifest))
        {
            return RelaunchUninstallerFromTemp(options, adjacentManifest);
        }

        if (!options.Quiet)
        {
            if (uninstall)
            {
                if (!ConfirmUninstall(options))
                {
                    return 5;
                }
            }
            else if (!ConfirmInstall(options))
            {
                return 5;
            }
        }

        var result = uninstall
            ? InstallerEngine.UninstallAsync(options).GetAwaiter().GetResult()
            : InstallerEngine.InstallAsync(options).GetAwaiter().GetResult();
        if (options.ReportFile is not null)
        {
            try
            {
                SetupJson.WriteAtomicAsync(options.ReportFile, result).GetAwaiter().GetResult();
            }
            catch (Exception ex)
            {
                if (!options.Quiet)
                {
                    MessageBox.Show($"{result.Message}\n\n但写入安装报告失败：{ex.Message}", ProductConstants.DisplayName, MessageBoxButtons.OK, MessageBoxIcon.Warning);
                }

                return result.Success ? 6 : result.ExitCode;
            }
        }

        if (!options.Quiet)
        {
            MessageBox.Show(
                result.Message,
                ProductConstants.DisplayName,
                MessageBoxButtons.OK,
                result.Success ? MessageBoxIcon.Information : MessageBoxIcon.Error);
        }

        if (result.Success && !uninstall && !options.Quiet && !options.NoLaunch)
        {
            LaunchInstalledApplication(options);
        }

        if (options.FromTemp)
        {
            ScheduleCurrentExecutableDeletion();
        }

        return result.ExitCode;
    }

    private static bool ConfirmInstall(SetupOptions options)
    {
        var message = $"将安装 {ProductConstants.DisplayName} {SetupJson.ProductVersion}。\n\n"
            + $"程序目录：{options.InstallDirectory}\n"
            + $"数据目录：{options.DataRoot}\n\n"
            + "程序只提供公开数据提醒和人工确认，不连接券商、不自动下单。\n"
            + "当前安装包未进行代码签名，Windows 可能显示未知发布者提示。\n\n继续安装吗？";
        return MessageBox.Show(message, ProductConstants.DisplayName, MessageBoxButtons.YesNo, MessageBoxIcon.Question, MessageBoxDefaultButton.Button2)
            == DialogResult.Yes;
    }

    private static bool ConfirmUninstall(SetupOptions options)
    {
        var message = options.DeleteData
            ? "将卸载程序，并删除本地数据库、设置、公告缓存、日志和备份。此操作不可撤销。继续吗？"
            : "将卸载程序。默认会保留本地数据库、设置、公告缓存和备份，方便以后重新安装。继续吗？";
        if (MessageBox.Show(message, ProductConstants.DisplayName, MessageBoxButtons.YesNo, MessageBoxIcon.Warning, MessageBoxDefaultButton.Button2)
            != DialogResult.Yes)
        {
            return false;
        }

        if (!options.DeleteData)
        {
            return true;
        }

        var secondConfirmation = MessageBox.Show(
            "再次确认：确定永久删除所有本地数据吗？\n\n如果只想卸载程序，请选择“否”，然后重新运行普通卸载。",
            "二次确认删除本地数据",
            MessageBoxButtons.YesNo,
            MessageBoxIcon.Stop,
            MessageBoxDefaultButton.Button2);
        if (secondConfirmation != DialogResult.Yes)
        {
            return false;
        }

        options.MarkDeleteDataConfirmed();
        return true;
    }

    private static int RelaunchUninstallerFromTemp(SetupOptions options, string manifestFile)
    {
        try
        {
            var currentExecutable = Environment.ProcessPath
                ?? throw new InvalidOperationException("无法确定卸载程序路径。");
            var temporaryExecutable = Path.Combine(
                Path.GetTempPath(),
                $"{ProductConstants.ProductId}-uninstall-{Guid.NewGuid():N}.exe");
            File.Copy(currentExecutable, temporaryExecutable, overwrite: false);
            var startInfo = new ProcessStartInfo(temporaryExecutable)
            {
                UseShellExecute = false,
                WorkingDirectory = Path.GetTempPath(),
            };
            foreach (var argument in options.BuildRelaunchArguments(manifestFile))
            {
                startInfo.ArgumentList.Add(argument);
            }

            Process.Start(startInfo);
            return 0;
        }
        catch (Exception ex)
        {
            if (!options.Quiet)
            {
                MessageBox.Show($"无法启动临时卸载程序：{ex.Message}", ProductConstants.DisplayName, MessageBoxButtons.OK, MessageBoxIcon.Error);
            }

            return 7;
        }
    }

    private static bool TryGetAdjacentManifest(out string manifestPath)
    {
        manifestPath = Path.Combine(AppContext.BaseDirectory, ProductConstants.InstallManifestName);
        return File.Exists(manifestPath);
    }

    private static void LaunchInstalledApplication(SetupOptions options)
    {
        try
        {
            var startInfo = new ProcessStartInfo(Path.Combine(options.InstallDirectory, ProductConstants.ExecutableName))
            {
                UseShellExecute = true,
                WorkingDirectory = options.InstallDirectory,
                Arguments = options.LaunchArguments,
            };
            Process.Start(startInfo);
        }
        catch (Exception ex)
        {
            MessageBox.Show($"安装已完成，但启动应用失败：{ex.Message}", ProductConstants.DisplayName, MessageBoxButtons.OK, MessageBoxIcon.Warning);
        }
    }

    private static void ScheduleCurrentExecutableDeletion()
    {
        var executable = Environment.ProcessPath;
        if (!string.IsNullOrWhiteSpace(executable))
        {
            _ = MoveFileEx(executable, null, MoveFileFlags.DelayUntilReboot);
        }
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool MoveFileEx(string existingFileName, string? newFileName, MoveFileFlags flags);

    [Flags]
    private enum MoveFileFlags : uint
    {
        DelayUntilReboot = 0x00000004,
    }
}
