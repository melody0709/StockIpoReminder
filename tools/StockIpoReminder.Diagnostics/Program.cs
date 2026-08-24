using System.Text;

namespace StockIpoReminder.Diagnostics;

public static class Program
{
    public static async Task<int> Main(string[] args)
    {
        Console.OutputEncoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
        var parseResult = DiagnosticOptions.Parse(args);
        if (parseResult.ShowHelp)
        {
            Console.WriteLine(DiagnosticOptions.Usage);
            return 0;
        }

        if (parseResult.Options is null)
        {
            Console.Error.WriteLine(parseResult.Error);
            Console.Error.WriteLine(DiagnosticOptions.Usage);
            return 2;
        }

        try
        {
            var report = await new DiagnosticRunner(parseResult.Options).RunAsync().ConfigureAwait(false);
            await DiagnosticOutput.WriteAsync(report, parseResult.Options.OutputPath).ConfigureAwait(false);
            return report.Success ? 0 : 1;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"诊断工具自身失败：{ex.Message}");
            return 3;
        }
    }
}
