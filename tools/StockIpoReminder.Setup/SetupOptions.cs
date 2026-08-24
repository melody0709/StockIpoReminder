using System.Globalization;
using System.Security.Cryptography;
using System.Text;

namespace StockIpoReminder.Setup;

internal sealed class SetupOptions
{
    public const string DefaultInstanceId = "default";

    private SetupOptions()
    {
    }

    public bool Uninstall { get; private set; }

    public bool Quiet { get; private set; }

    public bool NoLaunch { get; private set; }

    public bool DeleteData { get; private set; }

    public bool ConfirmDeleteData { get; private set; }

    public bool FromTemp { get; private set; }

    public string InstallDirectory { get; private set; } = DefaultInstallDirectory;

    public string DataRoot { get; private set; } = DefaultDataRoot;

    public string InstanceId { get; private set; } = DefaultInstanceId;

    public string? ReportFile { get; private set; }

    public string? ManifestFile { get; private set; }

    public bool IsDefaultInstance => string.Equals(InstanceId, DefaultInstanceId, StringComparison.OrdinalIgnoreCase);

    public string DisplayName => IsDefaultInstance
        ? ProductConstants.DisplayName
        : $"{ProductConstants.DisplayName} ({InstanceId})";

    public string RegistryKeyName => IsDefaultInstance
        ? ProductConstants.ProductId
        : $"{ProductConstants.ProductId}-{InstanceId}";

    public string StartMenuShortcutName => IsDefaultInstance
        ? $"{ProductConstants.DisplayName}.lnk"
        : $"{ProductConstants.DisplayName} ({InstanceId}).lnk";

    public string AutoStartTaskName => CreateAutoStartTaskName(DataRoot);

    public string LaunchArguments => PathsEqual(DataRoot, DefaultDataRoot)
        ? string.Empty
        : $"--data-root {QuoteWindowsArgument(DataRoot)}";

    public static string DefaultInstallDirectory => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Programs",
        ProductConstants.ProductId);

    public static string DefaultDataRoot => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        ProductConstants.ProductId);

    public static SetupOptions Parse(IReadOnlyList<string> arguments)
    {
        var options = new SetupOptions();
        for (var index = 0; index < arguments.Count; index++)
        {
            var argument = arguments[index];
            switch (argument.ToLowerInvariant())
            {
                case "--uninstall":
                    options.Uninstall = true;
                    continue;
                case "--quiet":
                    options.Quiet = true;
                    continue;
                case "--no-launch":
                    options.NoLaunch = true;
                    continue;
                case "--delete-data":
                    options.DeleteData = true;
                    continue;
                case "--confirm-delete-data":
                    options.ConfirmDeleteData = true;
                    continue;
                case "--from-temp":
                    options.FromTemp = true;
                    continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--install-dir", out var installDirectory))
            {
                options.InstallDirectory = NormalizePath(installDirectory, "--install-dir");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--data-root", out var dataRoot))
            {
                options.DataRoot = NormalizePath(dataRoot, "--data-root");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--instance-id", out var instanceId))
            {
                options.InstanceId = NormalizeInstanceId(instanceId);
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--report-file", out var reportFile))
            {
                options.ReportFile = NormalizePath(reportFile, "--report-file");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--manifest-file", out var manifestFile))
            {
                options.ManifestFile = NormalizePath(manifestFile, "--manifest-file");
                continue;
            }

            throw new ArgumentException($"未知安装参数：{argument}", nameof(arguments));
        }

        options.InstallDirectory = NormalizePath(options.InstallDirectory, nameof(InstallDirectory));
        options.DataRoot = NormalizePath(options.DataRoot, nameof(DataRoot));
        if (options.DeleteData && !options.Uninstall)
        {
            throw new ArgumentException("--delete-data 只能与 --uninstall 一起使用。", nameof(arguments));
        }

        return options;
    }

    public IReadOnlyList<string> BuildRelaunchArguments(string manifestFile)
    {
        var arguments = new List<string>
        {
            "--uninstall",
            "--from-temp",
            "--manifest-file",
            manifestFile,
        };
        if (Quiet)
        {
            arguments.Add("--quiet");
        }

        if (DeleteData)
        {
            arguments.Add("--delete-data");
        }

        if (ConfirmDeleteData)
        {
            arguments.Add("--confirm-delete-data");
        }

        if (ReportFile is not null)
        {
            arguments.Add("--report-file");
            arguments.Add(ReportFile);
        }

        return arguments;
    }

    public void MarkDeleteDataConfirmed() => ConfirmDeleteData = true;

    public static string CreateAutoStartTaskName(string dataRoot)
    {
        var normalizedDataRoot = NormalizePath(dataRoot, nameof(dataRoot));
        if (PathsEqual(normalizedDataRoot, DefaultDataRoot))
        {
            return ProductConstants.ProductId;
        }

        var key = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(normalizedDataRoot.ToUpperInvariant())))
            .ToLowerInvariant();
        return $"{ProductConstants.ProductId}-{key[..12]}";
    }

    public static bool PathsEqual(string first, string second) => string.Equals(
        NormalizePath(first, nameof(first)),
        NormalizePath(second, nameof(second)),
        StringComparison.OrdinalIgnoreCase);

    public static string QuoteWindowsArgument(string argument)
    {
        if (argument.Length > 0 && argument.All(static character => !char.IsWhiteSpace(character) && character != '"'))
        {
            return argument;
        }

        var builder = new StringBuilder(argument.Length + 2);
        builder.Append('"');
        var backslashCount = 0;
        foreach (var character in argument)
        {
            if (character == '\\')
            {
                backslashCount++;
                continue;
            }

            if (character == '"')
            {
                builder.Append('\\', (backslashCount * 2) + 1);
                builder.Append('"');
                backslashCount = 0;
                continue;
            }

            builder.Append('\\', backslashCount);
            builder.Append(character);
            backslashCount = 0;
        }

        builder.Append('\\', backslashCount * 2);
        builder.Append('"');
        return builder.ToString();
    }

    private static bool TryReadValue(
        IReadOnlyList<string> arguments,
        ref int index,
        string argument,
        string optionName,
        out string value)
    {
        if (string.Equals(argument, optionName, StringComparison.OrdinalIgnoreCase))
        {
            if (index + 1 >= arguments.Count || string.IsNullOrWhiteSpace(arguments[index + 1]))
            {
                throw new ArgumentException($"{optionName} 缺少参数。", nameof(arguments));
            }

            value = arguments[++index];
            return true;
        }

        var prefix = optionName + "=";
        if (argument.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
        {
            value = argument[prefix.Length..];
            if (string.IsNullOrWhiteSpace(value))
            {
                throw new ArgumentException($"{optionName} 缺少参数。", nameof(arguments));
            }

            return true;
        }

        value = string.Empty;
        return false;
    }

    private static string NormalizePath(string path, string parameterName)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path, parameterName);
        return Path.TrimEndingDirectorySeparator(Path.GetFullPath(Environment.ExpandEnvironmentVariables(path.Trim().Trim('"'))));
    }

    private static string NormalizeInstanceId(string value)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(value);
        var normalized = value.Trim();
        if (normalized.Length > 40 || normalized.Any(static character => !char.IsAsciiLetterOrDigit(character) && character is not '-' and not '_' and not '.'))
        {
            throw new ArgumentException("--instance-id 仅允许 1-40 个字母、数字、点、下划线或连字符。", nameof(value));
        }

        return normalized;
    }
}

internal static class ProductConstants
{
    public const string ProductId = "StockIpoReminder";
    public const string DisplayName = "A 股新股申购提醒";
    public const string Publisher = "StockIpoReminder";
    public const string ExecutableName = "StockIpoReminder.exe";
    public const string UninstallerName = "StockIpoReminder.Uninstaller.exe";
    public const string InstallManifestName = "install-manifest.json";
    public const string DataMarkerName = ".stock-ipo-reminder-data.json";
    public const string PayloadResourceName = "StockIpoReminder.Payload.zip";
}
