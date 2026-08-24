using System.Globalization;

namespace StockIpoReminder.Diagnostics;

public sealed record DiagnosticOptions
{
    public const string Usage = """
        用法：
          StockIpoReminder.Diagnostics [--sync] [--bse-sample] [--all]
                                       [--timeout-seconds 180] [--output <report.json>] [--keep]

        默认只运行四来源真实同步；--bse-sample 运行固定北交所公告样本；--all 运行两者。
        所有模式都使用唯一临时数据目录和临时 SQLite，不会启动后台 Host。
        临时目录默认在结束后删除；--keep 会保留并在报告中显示完整路径。
        """;

    public bool RunSync { get; init; }
    public bool RunBseSample { get; init; }
    public bool KeepTemporaryData { get; init; }
    public int TimeoutSeconds { get; init; } = 180;
    public string? OutputPath { get; init; }

    public string Mode => RunSync && RunBseSample
        ? "all"
        : RunBseSample
            ? "bse-sample"
            : "sync";

    public static DiagnosticOptionsParseResult Parse(IReadOnlyList<string> args)
    {
        var runSync = false;
        var runBse = false;
        var modeSpecified = false;
        var keep = false;
        var timeout = 180;
        string? output = null;

        for (var index = 0; index < args.Count; index++)
        {
            var argument = args[index];
            switch (argument.ToLowerInvariant())
            {
                case "--help":
                case "-h":
                case "/?":
                    return new DiagnosticOptionsParseResult { ShowHelp = true };
                case "--sync":
                    runSync = true;
                    modeSpecified = true;
                    break;
                case "--bse-sample":
                    runBse = true;
                    modeSpecified = true;
                    break;
                case "--all":
                    runSync = true;
                    runBse = true;
                    modeSpecified = true;
                    break;
                case "--keep":
                    keep = true;
                    break;
                case "--timeout-seconds":
                    if (++index >= args.Count
                        || !int.TryParse(args[index], NumberStyles.None, CultureInfo.InvariantCulture, out timeout)
                        || timeout is < 30 or > 900)
                    {
                        return DiagnosticOptionsParseResult.Fail("--timeout-seconds 必须是 30 到 900 之间的整数。");
                    }

                    break;
                case "--output":
                    if (++index >= args.Count || string.IsNullOrWhiteSpace(args[index]))
                    {
                        return DiagnosticOptionsParseResult.Fail("--output 后必须提供 JSON 报告路径。");
                    }

                    output = Path.GetFullPath(args[index]);
                    break;
                default:
                    return DiagnosticOptionsParseResult.Fail($"未知参数：{argument}");
            }
        }

        if (!modeSpecified)
        {
            runSync = true;
        }

        return new DiagnosticOptionsParseResult
        {
            Options = new DiagnosticOptions
            {
                RunSync = runSync,
                RunBseSample = runBse,
                KeepTemporaryData = keep,
                TimeoutSeconds = timeout,
                OutputPath = output,
            },
        };
    }
}

public sealed record DiagnosticOptionsParseResult
{
    public DiagnosticOptions? Options { get; init; }
    public string? Error { get; init; }
    public bool ShowHelp { get; init; }

    public static DiagnosticOptionsParseResult Fail(string error) => new() { Error = error };
}
