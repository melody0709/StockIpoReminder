using System.Globalization;
using System.Text.RegularExpressions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Announcements;

public sealed partial class AnnouncementFieldParser
{
    public const string Version = "2";

    public IReadOnlyList<ParsedAnnouncementField> Parse(string text, string title)
    {
        var compact = Whitespace().Replace(text, " ");
        var fields = new List<ParsedAnnouncementField>();
        AddMatch(fields, compact, "SecurityCode", SecurityCode(), 0.98m);
        AddApplyCodeMatch(fields, compact);
        AddDateMatch(fields, compact);
        AddDecimalMatch(fields, compact, "IssuePrice", IssuePrice(), 0.98m);
        AddQuantityMatch(fields, compact, "MaxApplyQuantity", MaxQuantity(), 0.92m);
        AddIntegerMatch(fields, compact, "LotSize", LotSize(), 0.95m);
        AddSessionMatch(fields, compact);
        AddFundingModeMatch(fields, compact);

        AddStatusMatch(fields, compact, title);

        return fields;
    }

    private static void AddMatch(ICollection<ParsedAnnouncementField> fields, string text, string name, Regex regex, decimal confidence)
    {
        var match = regex.Match(text);
        if (!match.Success)
        {
            return;
        }

        fields.Add(new ParsedAnnouncementField
        {
            Name = name,
            Value = match.Groups[1].Value,
            Confidence = confidence,
            Evidence = TrimEvidence(match.Value),
            CharacterOffset = match.Index,
        });
    }

    private static void AddDateMatch(ICollection<ParsedAnnouncementField> fields, string text)
    {
        var match = ApplyDate().Match(text);
        if (!match.Success)
        {
            return;
        }

        var date = ValueNormalizer.Date(match.Groups[1].Value);
        if (date is not null)
        {
            fields.Add(new ParsedAnnouncementField
            {
                Name = "ApplyDate",
                Value = date.Value.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
                Confidence = 0.98m,
                Evidence = TrimEvidence(match.Value),
                CharacterOffset = match.Index,
            });
        }
    }

    private static void AddApplyCodeMatch(ICollection<ParsedAnnouncementField> fields, string text)
    {
        var match = ApplyCode().Match(text);
        if (!match.Success)
        {
            match = SecurityCodeUsedForSubscription().Match(text);
        }

        if (!match.Success)
        {
            return;
        }

        fields.Add(new ParsedAnnouncementField
        {
            Name = "ApplyCode",
            Value = match.Groups[1].Value,
            Confidence = 0.99m,
            Evidence = TrimEvidence(match.Value),
            CharacterOffset = match.Index,
        });
    }

    private static void AddDecimalMatch(ICollection<ParsedAnnouncementField> fields, string text, string name, Regex regex, decimal confidence)
    {
        var match = regex.Match(text);
        var value = match.Success ? ValueNormalizer.Decimal(match.Groups[1].Value) : null;
        if (value is not null)
        {
            fields.Add(new ParsedAnnouncementField
            {
                Name = name,
                Value = value.Value.ToString(CultureInfo.InvariantCulture),
                Confidence = confidence,
                Evidence = TrimEvidence(match.Value),
                CharacterOffset = match.Index,
            });
        }
    }

    private static void AddQuantityMatch(ICollection<ParsedAnnouncementField> fields, string text, string name, Regex regex, decimal confidence)
    {
        var match = regex.Match(text);
        if (!match.Success)
        {
            return;
        }

        var raw = ValueNormalizer.Decimal(match.Groups[1].Value.Replace(",", string.Empty, StringComparison.Ordinal));
        if (raw is null)
        {
            return;
        }

        var multiplier = match.Groups[2].Value == "万" ? 10_000m : 1m;
        var value = decimal.ToInt32(decimal.Round(raw.Value * multiplier, 0, MidpointRounding.AwayFromZero));
        fields.Add(new ParsedAnnouncementField
        {
            Name = name,
            Value = value.ToString(CultureInfo.InvariantCulture),
            Confidence = confidence,
            Evidence = TrimEvidence(match.Value),
            CharacterOffset = match.Index,
        });
    }

    private static void AddIntegerMatch(ICollection<ParsedAnnouncementField> fields, string text, string name, Regex regex, decimal confidence)
    {
        var match = regex.Match(text);
        var valueGroup = match.Success && match.Groups["value"].Success ? match.Groups["value"] : match.Groups[1];
        if (match.Success && int.TryParse(valueGroup.Value, NumberStyles.Integer, CultureInfo.InvariantCulture, out var value))
        {
            fields.Add(new ParsedAnnouncementField
            {
                Name = name,
                Value = value.ToString(CultureInfo.InvariantCulture),
                Confidence = confidence,
                Evidence = TrimEvidence(match.Value),
                CharacterOffset = match.Index,
            });
        }
    }

    private static void AddSessionMatch(ICollection<ParsedAnnouncementField> fields, string text)
    {
        var match = Sessions().Match(text);
        if (match.Success)
        {
            fields.Add(new ParsedAnnouncementField
            {
                Name = "OfficialSessions",
                Value = $"{match.Groups[1].Value}-{match.Groups[2].Value},{match.Groups[3].Value}-{match.Groups[4].Value}",
                Confidence = 0.95m,
                Evidence = TrimEvidence(match.Value),
                CharacterOffset = match.Index,
            });
        }
    }

    private static void AddFundingModeMatch(ICollection<ParsedAnnouncementField> fields, string text)
    {
        var match = FullCashFunding().Match(text);
        if (!match.Success)
        {
            return;
        }

        fields.Add(new ParsedAnnouncementField
        {
            Name = "FundingMode",
            Value = FundingMode.FullCash.ToString(),
            Confidence = 0.99m,
            Evidence = TrimEvidence(match.Value),
            CharacterOffset = match.Index,
        });
    }

    private static void AddStatusMatch(ICollection<ParsedAnnouncementField> fields, string text, string title)
    {
        foreach (var status in new[] { "重新启动发行", "终止发行", "中止发行", "暂缓发行", "延期发行" })
        {
            var titleIndex = title.IndexOf(status, StringComparison.Ordinal);
            if (titleIndex >= 0)
            {
                fields.Add(new ParsedAnnouncementField
                {
                    Name = "IssueStatus",
                    Value = status,
                    Confidence = 0.99m,
                    Evidence = TrimEvidence(title),
                    CharacterOffset = titleIndex,
                });
                return;
            }
        }

        var match = ExplicitStatus().Match(text);
        if (match.Success)
        {
            var status = match.Groups["status"].Value;
            if (!status.EndsWith("发行", StringComparison.Ordinal))
            {
                status += "发行";
            }

            fields.Add(new ParsedAnnouncementField
            {
                Name = "IssueStatus",
                Value = status,
                Confidence = 0.92m,
                Evidence = TrimEvidence(match.Value),
                CharacterOffset = match.Index,
            });
        }
    }

    private static string TrimEvidence(string value) => value.Length <= 180 ? value : value[..180];

    [GeneratedRegex(@"\s+", RegexOptions.CultureInvariant)]
    private static partial Regex Whitespace();

    [GeneratedRegex(@"(?:股票代码|证券代码)\s*[：:]?\s*(\d{6})", RegexOptions.CultureInvariant)]
    private static partial Regex SecurityCode();

    [GeneratedRegex(@"申购代码\s*[：:]?\s*(\d{6})", RegexOptions.CultureInvariant)]
    private static partial Regex ApplyCode();

    [GeneratedRegex(@"(?:使用\s*证券代码|证券代码\s*为)\s*[“”\u0022']?(\d{6})[“”\u0022']?(?:\s*进行(?:网上)?申购)?", RegexOptions.CultureInvariant)]
    private static partial Regex SecurityCodeUsedForSubscription();

    [GeneratedRegex(@"(?:网上申购日期|网上申购日|申购日期|申购日)\s*(?:为|是|[：:])?\s*((?:20\d{2})[年\-/]\s*\d{1,2}[月\-/]\s*\d{1,2}日?)", RegexOptions.CultureInvariant)]
    private static partial Regex ApplyDate();

    [GeneratedRegex(@"(?:发行价格|发行价)\s*(?:为|是|[：:])?\s*(?:人民币\s*)?(\d+(?:\.\d+)?)\s*元", RegexOptions.CultureInvariant)]
    private static partial Regex IssuePrice();

    [GeneratedRegex(@"(?:网上申购上限|申购数量上限|申购上限|最高申购数量).{0,120}?([\d,.]+)\s*(万)?股", RegexOptions.CultureInvariant | RegexOptions.Singleline)]
    private static partial Regex MaxQuantity();

    [GeneratedRegex(@"(?:每\s*(?<value>\d+)\s*股\s*(?:为|作为)\s*一个申购单位|每(?:一|1)个申购单位\s*(?:为|是|[：:])?\s*(?<value>\d+)\s*股)", RegexOptions.CultureInvariant)]
    private static partial Regex LotSize();

    [GeneratedRegex(@"(?:网上)?申购时间\s*(?:为|是|[：:])?[^\d]{0,80}(\d{1,2}:\d{2})\s*[-—–至]\s*(\d{1,2}:\d{2})[^\d]{0,80}(\d{1,2}:\d{2})\s*[-—–至]\s*(\d{1,2}:\d{2})", RegexOptions.CultureInvariant)]
    private static partial Regex Sessions();

    [GeneratedRegex(@"(?:参与网上申购时|网上申购时|申购时).{0,40}?(?:需|须|应当|必须)?\s*(?:全额|足额)(?:缴付|缴纳).{0,24}?申购资金", RegexOptions.CultureInvariant | RegexOptions.Singleline)]
    private static partial Regex FullCashFunding();

    [GeneratedRegex(@"(?:决定|确认|公告称|现将)\s*(?<status>重新启动发行|终止发行|中止发行|暂缓发行|延期发行)|(?<status>终止|中止|暂缓|延期)本次发行|本次发行(?:决定)?\s*(?<status>重新启动|终止|中止|暂缓|延期)(?:发行)?", RegexOptions.CultureInvariant)]
    private static partial Regex ExplicitStatus();
}
