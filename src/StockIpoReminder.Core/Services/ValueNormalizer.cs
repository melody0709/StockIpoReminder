using System.Globalization;
using System.Security.Cryptography;
using System.Text;

namespace StockIpoReminder.Core.Services;

public static class ValueNormalizer
{
    private static readonly HashSet<string> Missing = new(StringComparer.OrdinalIgnoreCase)
    {
        string.Empty,
        "-",
        "--",
        "N/A",
        "NULL",
        "无",
    };

    public static string? Text(string? value)
    {
        var trimmed = value?.Trim();
        return trimmed is null || Missing.Contains(trimmed) ? null : trimmed;
    }

    public static decimal? Decimal(string? value, bool zeroMeansMissing = false)
    {
        var normalized = Text(value)?.Replace(",", string.Empty, StringComparison.Ordinal);
        if (!decimal.TryParse(normalized, NumberStyles.Number, CultureInfo.InvariantCulture, out var parsed))
        {
            return null;
        }

        return zeroMeansMissing && parsed == 0 ? null : parsed;
    }

    public static int? Integer(string? value, decimal multiplier = 1m, bool zeroMeansMissing = false)
    {
        var number = Decimal(value, zeroMeansMissing);
        if (number is null)
        {
            return null;
        }

        var result = number.Value * multiplier;
        return result is <= int.MaxValue and >= int.MinValue
            ? decimal.ToInt32(decimal.Round(result, 0, MidpointRounding.AwayFromZero))
            : null;
    }

    public static DateOnly? Date(string? value)
    {
        var normalized = Text(value);
        if (normalized is null)
        {
            return null;
        }

        var formats = new[] { "yyyy-MM-dd HH:mm:ss", "yyyy-MM-dd", "yyyy/MM/dd", "yyyyMMdd", "yyyy年M月d日" };
        return DateTime.TryParseExact(normalized, formats, CultureInfo.GetCultureInfo("zh-CN"), DateTimeStyles.AllowWhiteSpaces, out var parsed)
            || DateTime.TryParse(normalized, CultureInfo.GetCultureInfo("zh-CN"), DateTimeStyles.AllowWhiteSpaces, out parsed)
            ? DateOnly.FromDateTime(parsed)
            : null;
    }

    public static string Sha256(string value) => Sha256(Encoding.UTF8.GetBytes(value));

    public static string Sha256(ReadOnlySpan<byte> value) => Convert.ToHexString(SHA256.HashData(value)).ToLowerInvariant();
}
