using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using Microsoft.Win32;

namespace StockIpoReminder.Setup;

internal static class WindowsIntegration
{
    private const string UninstallRegistryRoot = @"Software\Microsoft\Windows\CurrentVersion\Uninstall";

    public static string GetStartMenuShortcutPath(string shortcutName) => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "Microsoft",
        "Windows",
        "Start Menu",
        "Programs",
        shortcutName);

    public static void CreateStartMenuShortcut(InstallationManifest manifest)
    {
        var executablePath = Path.Combine(manifest.InstallDirectory, ProductConstants.ExecutableName);
        Directory.CreateDirectory(Path.GetDirectoryName(manifest.StartMenuShortcutPath)!);
        CreateShortcut(
            manifest.StartMenuShortcutPath,
            executablePath,
            SetupOptions.PathsEqual(manifest.DataRoot, SetupOptions.DefaultDataRoot)
                ? string.Empty
                : $"--data-root {SetupOptions.QuoteWindowsArgument(manifest.DataRoot)}",
            manifest.InstallDirectory,
            manifest.DisplayName,
            executablePath);
    }

    public static void RegisterUninstall(InstallationManifest manifest)
    {
        var uninstallExecutable = Path.Combine(manifest.InstallDirectory, ProductConstants.UninstallerName);
        var displayIcon = Path.Combine(manifest.InstallDirectory, ProductConstants.ExecutableName);
        using var root = Registry.CurrentUser.CreateSubKey(UninstallRegistryRoot, writable: true)
            ?? throw new InvalidOperationException("无法打开当前用户卸载注册表位置。");
        using var key = root.CreateSubKey(manifest.RegistryKeyName, writable: true)
            ?? throw new InvalidOperationException("无法创建当前用户卸载注册表项。");
        key.SetValue("DisplayName", manifest.DisplayName, RegistryValueKind.String);
        key.SetValue("DisplayVersion", manifest.Version, RegistryValueKind.String);
        key.SetValue("Publisher", ProductConstants.Publisher, RegistryValueKind.String);
        key.SetValue("InstallLocation", manifest.InstallDirectory, RegistryValueKind.String);
        key.SetValue("DisplayIcon", displayIcon, RegistryValueKind.String);
        key.SetValue("UninstallString", $"{SetupOptions.QuoteWindowsArgument(uninstallExecutable)} --uninstall", RegistryValueKind.String);
        key.SetValue("QuietUninstallString", $"{SetupOptions.QuoteWindowsArgument(uninstallExecutable)} --uninstall --quiet", RegistryValueKind.String);
        key.SetValue("NoModify", 1, RegistryValueKind.DWord);
        key.SetValue("NoRepair", 1, RegistryValueKind.DWord);
        key.SetValue("EstimatedSize", checked((int)Math.Min(int.MaxValue, GetDirectorySize(manifest.InstallDirectory) / 1024)), RegistryValueKind.DWord);
        key.SetValue("InstallDate", DateTime.Now.ToString("yyyyMMdd", System.Globalization.CultureInfo.InvariantCulture), RegistryValueKind.String);
    }

    public static void RemoveStartMenuShortcut(string shortcutPath)
    {
        if (File.Exists(shortcutPath))
        {
            File.Delete(shortcutPath);
        }
    }

    public static void RemoveUninstallRegistration(string registryKeyName)
    {
        using var root = Registry.CurrentUser.OpenSubKey(UninstallRegistryRoot, writable: true);
        root?.DeleteSubKeyTree(registryKeyName, throwOnMissingSubKey: false);
    }

    public static async Task DeleteScheduledTaskAsync(string taskName, CancellationToken cancellationToken = default)
    {
        var startInfo = new ProcessStartInfo("schtasks.exe")
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        startInfo.ArgumentList.Add("/Delete");
        startInfo.ArgumentList.Add("/TN");
        startInfo.ArgumentList.Add(taskName);
        startInfo.ArgumentList.Add("/F");
        using var process = Process.Start(startInfo) ?? throw new InvalidOperationException("无法启动 schtasks.exe。");
        await process.WaitForExitAsync(cancellationToken).ConfigureAwait(false);
        if (process.ExitCode is not 0 and not 1)
        {
            var error = await process.StandardError.ReadToEndAsync(cancellationToken).ConfigureAwait(false);
            throw new InvalidOperationException($"删除登录自启动计划任务失败，退出码 {process.ExitCode}：{error.Trim()}");
        }
    }

    public static bool IsApplicationRunning(string installDirectory)
    {
        var expectedExecutable = Path.GetFullPath(Path.Combine(installDirectory, ProductConstants.ExecutableName));
        foreach (var process in Process.GetProcessesByName(Path.GetFileNameWithoutExtension(ProductConstants.ExecutableName)))
        {
            using (process)
            {
                try
                {
                    var processPath = process.MainModule?.FileName;
                    if (!string.IsNullOrWhiteSpace(processPath)
                        && string.Equals(Path.GetFullPath(processPath), expectedExecutable, StringComparison.OrdinalIgnoreCase))
                    {
                        return true;
                    }
                }
                catch (Exception ex) when (ex is InvalidOperationException or System.ComponentModel.Win32Exception or NotSupportedException)
                {
                }
            }
        }

        return false;
    }

    private static long GetDirectorySize(string directory)
    {
        if (!Directory.Exists(directory))
        {
            return 0;
        }

        return Directory.EnumerateFiles(directory, "*", SearchOption.AllDirectories)
            .Sum(static path => new FileInfo(path).Length);
    }

    private static void CreateShortcut(
        string shortcutPath,
        string targetPath,
        string arguments,
        string workingDirectory,
        string description,
        string iconPath)
    {
        var shellLink = (IShellLinkW)(object)new ShellLink();
        try
        {
            shellLink.SetPath(targetPath);
            shellLink.SetArguments(arguments);
            shellLink.SetWorkingDirectory(workingDirectory);
            shellLink.SetDescription(description);
            shellLink.SetIconLocation(iconPath, 0);
            ((IPersistFile)shellLink).Save(shortcutPath, false);
        }
        finally
        {
            Marshal.FinalReleaseComObject(shellLink);
        }
    }

    [ComImport]
    [Guid("00021401-0000-0000-C000-000000000046")]
    private sealed class ShellLink;

    [ComImport]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    [Guid("000214F9-0000-0000-C000-000000000046")]
    private interface IShellLinkW
    {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder file, int maxPath, nint findData, uint flags);
        void GetIDList(out nint idList);
        void SetIDList(nint idList);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder name, int maxName);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string name);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder directory, int maxPath);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string directory);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder arguments, int maxPath);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
        void GetHotkey(out short hotkey);
        void SetHotkey(short hotkey);
        void GetShowCmd(out int showCommand);
        void SetShowCmd(int showCommand);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder iconPath, int iconPathLength, out int iconIndex);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string iconPath, int iconIndex);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
        void Resolve(nint windowHandle, uint flags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
    }
}
