using System.Text.RegularExpressions;

namespace StockIpoReminder.Infrastructure.Operations;

public static partial class DiagnosticRedactor
{
    public static string Redact(string? value)
    {
        if (string.IsNullOrEmpty(value))
        {
            return value ?? string.Empty;
        }

        var redacted = UrlQueryPattern().Replace(value, static match => $"{match.Groups["url"].Value}?<redacted>");
        redacted = HeaderPattern().Replace(redacted, static match => $"{match.Groups["name"].Value}: <redacted>");
        return SecretPattern().Replace(redacted, static match => $"{match.Groups["prefix"].Value}\"<redacted>\"");
    }

    [GeneratedRegex(@"(?<url>https?://[^\s\""'<>?]+)\?[^\s\""'<>]*", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant)]
    private static partial Regex UrlQueryPattern();

    [GeneratedRegex(@"(?im)\b(?<name>authorization|proxy-authorization|cookie|set-cookie)\s*[:=]\s*[^\r\n""]+", RegexOptions.CultureInvariant)]
    private static partial Regex HeaderPattern();

    [GeneratedRegex(@"(?i)(?<prefix>[\""']?(?:password|passwd|token|secret|api[_-]?key|access[_-]?key)[\""']?\s*[:=]\s*)(?:""[^""]*""|'[^']*'|[^,;\s\r\n}\]]+)", RegexOptions.CultureInvariant)]
    private static partial Regex SecretPattern();
}
