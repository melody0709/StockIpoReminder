using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Text;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed class ApplicationRuntimeOptions
{
    public const string DataRootEnvironmentVariable = "STOCK_IPO_REMINDER_DATA_ROOT";

    private ApplicationRuntimeOptions(
        string dataRoot,
        string defaultDataRoot,
        bool background,
        bool smokeMode,
        bool smokeEnableAutoStart,
        bool smokeSeedScenarios,
        string? readyFile,
        string? uiSmokeReport,
        ProcessSmokeStage? processSmokeStage,
        string? processSmokeReport,
        string? recoverySmokeReport,
        TimeSpan? exitAfter)
    {
        DataRoot = dataRoot;
        DefaultDataRoot = defaultDataRoot;
        Background = background;
        SmokeMode = smokeMode;
        SmokeEnableAutoStart = smokeEnableAutoStart;
        SmokeSeedScenarios = smokeSeedScenarios;
        ReadyFile = readyFile;
        UiSmokeReport = uiSmokeReport;
        ProcessSmokePhase = processSmokeStage;
        ProcessSmokeReport = processSmokeReport;
        RecoverySmokeReport = recoverySmokeReport;
        ExitAfter = exitAfter;
        UsesDefaultDataRoot = string.Equals(dataRoot, defaultDataRoot, StringComparison.OrdinalIgnoreCase);
        InstanceKey = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(dataRoot.ToUpperInvariant())))
            .ToLowerInvariant();
    }

    public string DataRoot { get; }

    public string DefaultDataRoot { get; }

    public bool UsesDefaultDataRoot { get; }

    public bool Background { get; }

    public bool SmokeMode { get; }

    public bool SmokeEnableAutoStart { get; }

    public bool SmokeSeedScenarios { get; }

    public string? ReadyFile { get; }

    public string? UiSmokeReport { get; }

    public ProcessSmokeStage? ProcessSmokePhase { get; }

    public string? ProcessSmokeReport { get; }

    public string? RecoverySmokeReport { get; }

    public TimeSpan? ExitAfter { get; }

    public string InstanceKey { get; }

    public string MutexName => $"Local\\StockIpoReminder.{InstanceKey}";

    public string AutoStartTaskName => UsesDefaultDataRoot
        ? "StockIpoReminder"
        : $"StockIpoReminder-{InstanceKey[..12]}";

    public string AutoStartArguments => UsesDefaultDataRoot
        ? "--background"
        : $"--background --data-root {QuoteWindowsArgument(DataRoot)}";

    public static ApplicationRuntimeOptions Parse(IReadOnlyList<string> arguments)
    {
        var defaultDataRoot = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "StockIpoReminder");
        return Parse(
            arguments,
            defaultDataRoot,
            Environment.GetEnvironmentVariable(DataRootEnvironmentVariable));
    }

    public static ApplicationRuntimeOptions Parse(
        IReadOnlyList<string> arguments,
        string defaultDataRoot,
        string? environmentDataRoot)
    {
        var normalizedDefaultRoot = NormalizePath(defaultDataRoot, nameof(defaultDataRoot));
        var dataRoot = string.IsNullOrWhiteSpace(environmentDataRoot)
            ? normalizedDefaultRoot
            : NormalizePath(environmentDataRoot, DataRootEnvironmentVariable);
        string? readyFile = null;
        string? uiSmokeReport = null;
        ProcessSmokeStage? processSmokeStage = null;
        string? processSmokeReport = null;
        string? recoverySmokeReport = null;
        TimeSpan? exitAfter = null;
        var background = false;
        var smokeMode = false;
        var smokeEnableAutoStart = false;
        var smokeSeedScenarios = false;

        for (var index = 0; index < arguments.Count; index++)
        {
            var argument = arguments[index];
            if (string.Equals(argument, "--background", StringComparison.OrdinalIgnoreCase))
            {
                background = true;
                continue;
            }

            if (string.Equals(argument, "--smoke-mode", StringComparison.OrdinalIgnoreCase))
            {
                smokeMode = true;
                continue;
            }

            if (string.Equals(argument, "--smoke-enable-autostart", StringComparison.OrdinalIgnoreCase))
            {
                smokeEnableAutoStart = true;
                continue;
            }

            if (string.Equals(argument, "--smoke-seed-scenarios", StringComparison.OrdinalIgnoreCase))
            {
                smokeSeedScenarios = true;
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--data-root", out var rootValue))
            {
                dataRoot = NormalizePath(rootValue, "--data-root");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--ready-file", out var readyValue))
            {
                readyFile = NormalizePath(readyValue, "--ready-file");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--ui-smoke-report", out var uiSmokeValue))
            {
                uiSmokeReport = NormalizePath(uiSmokeValue, "--ui-smoke-report");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--process-smoke-stage", out var processSmokeStageValue))
            {
                processSmokeStage = processSmokeStageValue.ToLowerInvariant() switch
                {
                    "prepare" => ProcessSmokeStage.Prepare,
                    "verify" => ProcessSmokeStage.Verify,
                    _ => throw new ArgumentException("--process-smoke-stage 必须是 prepare 或 verify。", nameof(arguments)),
                };
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--process-smoke-report", out var processSmokeReportValue))
            {
                processSmokeReport = NormalizePath(processSmokeReportValue, "--process-smoke-report");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--recovery-smoke-report", out var recoverySmokeReportValue))
            {
                recoverySmokeReport = NormalizePath(recoverySmokeReportValue, "--recovery-smoke-report");
                continue;
            }

            if (TryReadValue(arguments, ref index, argument, "--exit-after-seconds", out var secondsValue))
            {
                if (!double.TryParse(secondsValue, NumberStyles.AllowDecimalPoint, CultureInfo.InvariantCulture, out var seconds)
                    || seconds is < 1 or > 3600)
                {
                    throw new ArgumentException("--exit-after-seconds 必须是 1 到 3600 之间的数字。", nameof(arguments));
                }

                exitAfter = TimeSpan.FromSeconds(seconds);
            }
        }

        if (smokeEnableAutoStart && !smokeMode)
        {
            throw new ArgumentException("--smoke-enable-autostart 必须与 --smoke-mode 一起使用。", nameof(arguments));
        }

        if (smokeSeedScenarios && !smokeMode)
        {
            throw new ArgumentException("--smoke-seed-scenarios 必须与 --smoke-mode 一起使用。", nameof(arguments));
        }

        if (uiSmokeReport is not null && (!smokeMode || !smokeSeedScenarios))
        {
            throw new ArgumentException("--ui-smoke-report 必须与 --smoke-mode 和 --smoke-seed-scenarios 一起使用。", nameof(arguments));
        }

        if ((processSmokeStage is null) != (processSmokeReport is null))
        {
            throw new ArgumentException("--process-smoke-stage 与 --process-smoke-report 必须同时使用。", nameof(arguments));
        }

        if (processSmokeStage is not null && !smokeMode)
        {
            throw new ArgumentException("进程恢复 smoke 必须与 --smoke-mode 一起使用。", nameof(arguments));
        }

        if (processSmokeStage == ProcessSmokeStage.Prepare && !smokeSeedScenarios)
        {
            throw new ArgumentException("进程恢复 smoke 的 prepare 阶段必须使用 --smoke-seed-scenarios。", nameof(arguments));
        }

        if (recoverySmokeReport is not null && !smokeMode)
        {
            throw new ArgumentException("恢复事件 smoke 必须与 --smoke-mode 一起使用。", nameof(arguments));
        }

        var exclusiveSmokeCount = (uiSmokeReport is null ? 0 : 1)
            + (processSmokeReport is null ? 0 : 1)
            + (recoverySmokeReport is null ? 0 : 1);
        if (exclusiveSmokeCount > 1)
        {
            throw new ArgumentException("UI、进程恢复与恢复事件 smoke 不能在同一进程中运行。", nameof(arguments));
        }

        return new ApplicationRuntimeOptions(
            dataRoot,
            normalizedDefaultRoot,
            background,
            smokeMode,
            smokeEnableAutoStart,
            smokeSeedScenarios,
            readyFile,
            uiSmokeReport,
            processSmokeStage,
            processSmokeReport,
            recoverySmokeReport,
            exitAfter);
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
        var expanded = Environment.ExpandEnvironmentVariables(path.Trim().Trim('"'));
        return Path.TrimEndingDirectorySeparator(Path.GetFullPath(expanded));
    }

    private static string QuoteWindowsArgument(string argument)
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
}

public enum ProcessSmokeStage
{
    Prepare = 1,
    Verify = 2,
}
