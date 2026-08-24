using System.Globalization;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Infrastructure.Announcements;

public static class AnnouncementCandidateFactory
{
    public static IpoCandidate Create(IpoEvent existing, AnnouncementDocument document)
    {
        var fields = document.ParsedFields
            .Where(static x => x.Confidence >= 0.90m)
            .GroupBy(static x => x.Name, StringComparer.OrdinalIgnoreCase)
            .ToDictionary(static x => x.Key, static x => x.OrderByDescending(y => y.Confidence).First().Value, StringComparer.OrdinalIgnoreCase);

        var status = fields.GetValueOrDefault("IssueStatus") switch
        {
            "终止发行" => IssueStatus.Terminated,
            "中止发行" => IssueStatus.Suspended,
            "暂缓发行" or "延期发行" => IssueStatus.Postponed,
            "重新启动发行" => IssueStatus.Upcoming,
            _ => existing.Status,
        };

        return new IpoCandidate
        {
            Source = document.Reference.Provider,
            SourcePriority = 1000,
            FetchedAt = document.DownloadedAt,
            SourcePublishedAt = document.Reference.PublishedAt,
            Exchange = existing.Exchange,
            Board = existing.Board,
            SecurityCode = fields.GetValueOrDefault("SecurityCode") ?? existing.SecurityCode,
            ApplyCode = fields.GetValueOrDefault("ApplyCode"),
            LegacyCode = existing.LegacyCode,
            Name = existing.Name,
            ApplyDate = ValueNormalizer.Date(fields.GetValueOrDefault("ApplyDate")),
            IssuePrice = ValueNormalizer.Decimal(fields.GetValueOrDefault("IssuePrice"), zeroMeansMissing: true),
            LotSize = ValueNormalizer.Integer(fields.GetValueOrDefault("LotSize"), zeroMeansMissing: true),
            MaxApplyQuantity = ValueNormalizer.Integer(fields.GetValueOrDefault("MaxApplyQuantity"), zeroMeansMissing: true),
            Status = status,
            AnnouncementUrl = document.Reference.Url.ToString(),
            Sessions = ParseSessions(
                existing,
                fields.GetValueOrDefault("OfficialSessions"),
                fields.GetValueOrDefault("FundingMode")),
            IsAnnouncementDerived = true,
            Fields = document.ParsedFields.Select(field => new SourceFieldValue
            {
                FieldName = field.Name,
                RawValue = field.Evidence,
                NormalizedValue = field.Value,
                Source = document.Reference.Provider,
                Priority = 1000,
                SourcePublishedAt = document.Reference.PublishedAt,
                FetchedAt = document.DownloadedAt,
                RawHash = document.FileHash,
            }).ToArray(),
        };
    }

    private static IReadOnlyList<SubscriptionSession> ParseSessions(
        IpoEvent existing,
        string? value,
        string? fundingModeValue)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return [];
        }

        var fundingMode = Enum.TryParse<FundingMode>(fundingModeValue, ignoreCase: true, out var parsedFundingMode)
            ? parsedFundingMode
            : existing.Exchange == Exchange.Beijing
                ? FundingMode.FullCash
                : FundingMode.MarketValue;
        var pairs = value.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var result = new List<SubscriptionSession>();
        for (var i = 0; i < pairs.Length; i++)
        {
            var parts = pairs[i].Split('-', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            if (parts.Length == 2
                && TimeOnly.TryParseExact(parts[0], "H:mm", CultureInfo.InvariantCulture, DateTimeStyles.None, out var start)
                && TimeOnly.TryParseExact(parts[1], "H:mm", CultureInfo.InvariantCulture, DateTimeStyles.None, out var end))
            {
                result.Add(new SubscriptionSession
                {
                    SessionNumber = i + 1,
                    OfficialStart = start,
                    OfficialEnd = end,
                    FundingMode = fundingMode,
                    AllocationTimeSensitive = existing.Exchange == Exchange.Beijing,
                    Source = "announcement",
                });
            }
        }

        return result;
    }
}
