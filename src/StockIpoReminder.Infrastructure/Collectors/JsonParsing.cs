using System.Text.Json;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Collectors;

internal static class JsonParsing
{
    public static string? String(JsonElement element, string property)
    {
        if (!element.TryGetProperty(property, out var value) || value.ValueKind is JsonValueKind.Null or JsonValueKind.Undefined)
        {
            return null;
        }

        return ValueNormalizer.Text(value.ValueKind == JsonValueKind.String ? value.GetString() : value.GetRawText().Trim('"'));
    }

    public static decimal? Decimal(JsonElement element, string property, bool zeroMeansMissing = false) =>
        ValueNormalizer.Decimal(String(element, property), zeroMeansMissing);

    public static int? Integer(JsonElement element, string property, decimal multiplier = 1m, bool zeroMeansMissing = false) =>
        ValueNormalizer.Integer(String(element, property), multiplier, zeroMeansMissing);

    public static DateOnly? Date(JsonElement element, string property) => ValueNormalizer.Date(String(element, property));

    public static DateOnly? EpochDate(JsonElement element, string property)
    {
        if (!element.TryGetProperty(property, out var value) || value.ValueKind != JsonValueKind.Object
            || !value.TryGetProperty("time", out var epochElement) || !epochElement.TryGetInt64(out var epoch))
        {
            return null;
        }

        var instant = DateTimeOffset.FromUnixTimeMilliseconds(epoch);
        return DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(instant, ChinaTime.Zone).DateTime);
    }

    public static string SchemaFingerprint(IEnumerable<JsonElement> elements)
    {
        var names = elements
            .Where(static x => x.ValueKind == JsonValueKind.Object)
            .SelectMany(static x => x.EnumerateObject().Select(static p => p.Name))
            .Distinct(StringComparer.Ordinal)
            .Order(StringComparer.Ordinal);
        return ValueNormalizer.Sha256(string.Join("\n", names));
    }

    public static IssueStatus StatusFromDates(DateOnly? applyDate, DateOnly today, bool suspended = false, bool terminated = false)
    {
        if (terminated)
        {
            return IssueStatus.Terminated;
        }

        if (suspended)
        {
            return IssueStatus.Suspended;
        }

        if (applyDate is null)
        {
            return IssueStatus.Unknown;
        }

        return applyDate > today ? IssueStatus.Upcoming : applyDate == today ? IssueStatus.Active : IssueStatus.Completed;
    }

    public static Exchange DetectExchange(string? securityCode, string? marketText, bool isBeijing = false)
    {
        if (isBeijing || Contains(marketText, "北交") || Contains(marketText, "北京"))
        {
            return Exchange.Beijing;
        }

        if (Contains(marketText, "沪") || Contains(marketText, "上海") || Contains(marketText, "科创"))
        {
            return Exchange.Shanghai;
        }

        if (Contains(marketText, "深") || Contains(marketText, "创业"))
        {
            return Exchange.Shenzhen;
        }

        if (securityCode?.StartsWith('6') == true)
        {
            return Exchange.Shanghai;
        }

        if (securityCode?.StartsWith('8') == true || securityCode?.StartsWith('9') == true)
        {
            return Exchange.Beijing;
        }

        return string.IsNullOrWhiteSpace(securityCode) ? Exchange.Unknown : Exchange.Shenzhen;
    }

    public static Board DetectBoard(Exchange exchange, string? securityCode, string? marketText)
    {
        if (exchange == Exchange.Beijing)
        {
            return Board.Beijing;
        }

        if (Contains(marketText, "科创") || securityCode?.StartsWith("688", StringComparison.Ordinal) == true)
        {
            return Board.Star;
        }

        if (Contains(marketText, "创业") || securityCode?.StartsWith("30", StringComparison.Ordinal) == true)
        {
            return Board.ChiNext;
        }

        return exchange == Exchange.Unknown ? Board.Unknown : Board.Main;
    }

    private static bool Contains(string? value, string text) => value?.Contains(text, StringComparison.OrdinalIgnoreCase) == true;
}
