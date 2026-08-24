using System.Globalization;
using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Core.Services;

public sealed class IpoReconciler
{
    private static readonly string[] CriticalFields =
    [
        nameof(IpoCandidate.ApplyCode),
        nameof(IpoCandidate.ApplyDate),
        nameof(IpoCandidate.IssuePrice),
        nameof(IpoCandidate.Status),
    ];

    public ReconciledIpoEvent? Reconcile(
        IReadOnlyCollection<IpoCandidate> candidates,
        IpoEvent? existing,
        AppSettings settings,
        DateTimeOffset now)
    {
        var usable = candidates
            .Where(static x => x.StableIdentity is not null)
            .OrderByDescending(static x => x.IsAnnouncementDerived)
            .ThenByDescending(static x => x.SourcePriority)
            .ThenByDescending(static x => x.SourcePublishedAt)
            .ThenByDescending(static x => x.FetchedAt)
            .ToArray();
        if (usable.Length == 0)
        {
            return null;
        }

        var securityCode = Pick(usable, static x => x.SecurityCode) ?? Pick(usable, static x => x.ApplyCode) ?? existing?.SecurityCode;
        var name = Pick(usable, static x => x.Name) ?? existing?.Name;
        if (string.IsNullOrWhiteSpace(securityCode) || string.IsNullOrWhiteSpace(name))
        {
            return null;
        }

        var exchange = PickValue(usable, static x => x.Exchange, Exchange.Unknown);
        if (exchange == Exchange.Unknown && existing is not null)
        {
            exchange = existing.Exchange;
        }

        var board = PickValue(usable, static x => x.Board, Board.Unknown);
        if (board == Board.Unknown && existing is not null)
        {
            board = existing.Board;
        }

        var applyCode = Pick(usable, static x => x.ApplyCode) ?? existing?.ApplyCode;
        var applyDate = PickNullable(usable, static x => x.ApplyDate) ?? existing?.ApplyDate;
        var status = PickValue(usable, static x => x.Status, IssueStatus.Unknown);
        if (status == IssueStatus.Unknown && existing is not null)
        {
            status = existing.Status;
        }
        var sessions = usable.FirstOrDefault(static x => x.Sessions.Count > 0)?.Sessions
            ?? (existing?.Sessions.Count > 0 ? existing.Sessions : MarketSessionFactory.CreateDefault(exchange, settings));
        var conflicts = FindConflicts(usable);
        var independentSources = usable.Select(static x => x.Source).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        var announcementVerified = usable.Any(static x => x.IsAnnouncementDerived);
        var missingRequired = applyDate is not null
            && (string.IsNullOrWhiteSpace(applyCode) || sessions.Count == 0);

        var quality = missingRequired
            ? DataQualityStatus.ManualReviewRequired
            : conflicts.Count > 0
                ? DataQualityStatus.DataConflict
                : announcementVerified
                    ? DataQualityStatus.AnnouncementVerified
                    : independentSources > 1
                        ? DataQualityStatus.MultiSourceVerified
                        : DataQualityStatus.SingleSource;

        var lifecycle = ResolveLifecycle(existing, applyDate, status, now);
        var resolved = new IpoEvent
        {
            Id = existing?.Id ?? IpoEventIdentity.Create(exchange, securityCode),
            Exchange = exchange,
            Board = board,
            SecurityCode = securityCode,
            ApplyCode = applyCode,
            LegacyCode = Pick(usable, static x => x.LegacyCode) ?? existing?.LegacyCode,
            Name = name,
            ApplyDate = applyDate,
            IssuePrice = PickNullable(usable, static x => x.IssuePrice) ?? existing?.IssuePrice,
            LotSize = PickNullable(usable, static x => x.LotSize) ?? existing?.LotSize,
            MaxApplyQuantity = PickNullable(usable, static x => x.MaxApplyQuantity) ?? existing?.MaxApplyQuantity,
            RequiredMarketValue = PickNullable(usable, static x => x.RequiredMarketValue) ?? existing?.RequiredMarketValue,
            RequiredCash = PickNullable(usable, static x => x.RequiredCash) ?? existing?.RequiredCash,
            BallotDate = PickNullable(usable, static x => x.BallotDate) ?? existing?.BallotDate,
            PaymentDate = PickNullable(usable, static x => x.PaymentDate) ?? existing?.PaymentDate,
            ListingDate = PickNullable(usable, static x => x.ListingDate) ?? existing?.ListingDate,
            Status = status,
            LifecycleStatus = lifecycle,
            EventVersion = existing?.EventVersion ?? 1,
            AnnouncementUrl = Pick(usable, static x => x.AnnouncementUrl) ?? existing?.AnnouncementUrl,
            DataQualityStatus = quality,
            DataConflict = conflicts.Count > 0,
            FirstSeenAt = existing?.FirstSeenAt ?? now,
            UpdatedAt = now,
            Sessions = sessions,
        };

        var fieldSources = new List<SourceFieldValue>();
        foreach (var candidate in usable)
        {
            if (candidate.Fields.Count > 0)
            {
                fieldSources.AddRange(candidate.Fields);
            }
            else
            {
                fieldSources.AddRange(CreateFieldSources(candidate));
            }
        }

        return new ReconciledIpoEvent
        {
            Event = resolved,
            FieldSources = fieldSources,
            ConflictFields = conflicts,
        };
    }

    private static IpoLifecycleStatus ResolveLifecycle(
        IpoEvent? existing,
        DateOnly? applyDate,
        IssueStatus status,
        DateTimeOffset now)
    {
        if (status is IssueStatus.Suspended or IssueStatus.Terminated or IssueStatus.Postponed)
        {
            return IpoLifecycleStatus.SuspendedOrCancelled;
        }

        if (existing?.LifecycleStatus is IpoLifecycleStatus.Acknowledged or IpoLifecycleStatus.AcknowledgedNeedsReview)
        {
            return existing.LifecycleStatus;
        }

        if (applyDate is null)
        {
            return IpoLifecycleStatus.Discovered;
        }

        var today = DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(now, ChinaTime.Zone).DateTime);
        if (applyDate > today)
        {
            return IpoLifecycleStatus.Scheduled;
        }

        return applyDate == today
            ? IpoLifecycleStatus.ActiveUnconfirmed
            : existing?.LifecycleStatus == IpoLifecycleStatus.Acknowledged
                ? IpoLifecycleStatus.Acknowledged
                : IpoLifecycleStatus.ExpiredUnconfirmed;
    }

    private static IReadOnlyList<string> FindConflicts(IReadOnlyList<IpoCandidate> candidates)
    {
        var conflicts = new List<string>();
        foreach (var field in CriticalFields)
        {
            var values = field switch
            {
                nameof(IpoCandidate.ApplyCode) => candidates.Select(static x => x.ApplyCode),
                nameof(IpoCandidate.ApplyDate) => candidates.Select(static x => x.ApplyDate?.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture)),
                nameof(IpoCandidate.IssuePrice) => candidates.Select(static x => x.IssuePrice?.ToString(CultureInfo.InvariantCulture)),
                nameof(IpoCandidate.Status) => candidates.Select(static x => x.Status == IssueStatus.Unknown ? null : x.Status.ToString()),
                _ => [],
            };

            if (values.Where(static x => !string.IsNullOrWhiteSpace(x)).Distinct(StringComparer.OrdinalIgnoreCase).Skip(1).Any())
            {
                conflicts.Add(field);
            }
        }

        return conflicts;
    }

    private static IEnumerable<SourceFieldValue> CreateFieldSources(IpoCandidate candidate)
    {
        var fields = new Dictionary<string, string?>
        {
            [nameof(candidate.SecurityCode)] = candidate.SecurityCode,
            [nameof(candidate.ApplyCode)] = candidate.ApplyCode,
            [nameof(candidate.Name)] = candidate.Name,
            [nameof(candidate.ApplyDate)] = candidate.ApplyDate?.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            [nameof(candidate.IssuePrice)] = candidate.IssuePrice?.ToString(CultureInfo.InvariantCulture),
            [nameof(candidate.LotSize)] = candidate.LotSize?.ToString(CultureInfo.InvariantCulture),
            [nameof(candidate.MaxApplyQuantity)] = candidate.MaxApplyQuantity?.ToString(CultureInfo.InvariantCulture),
            [nameof(candidate.Status)] = candidate.Status.ToString(),
        };

        return fields
            .Where(static x => !string.IsNullOrWhiteSpace(x.Value))
            .Select(x => new SourceFieldValue
            {
                FieldName = x.Key,
                RawValue = x.Value,
                NormalizedValue = x.Value,
                Source = candidate.Source,
                Priority = candidate.SourcePriority,
                SourcePublishedAt = candidate.SourcePublishedAt,
                FetchedAt = candidate.FetchedAt,
            });
    }

    private static string? Pick(IReadOnlyList<IpoCandidate> candidates, Func<IpoCandidate, string?> selector) =>
        candidates.Select(selector).FirstOrDefault(static x => !string.IsNullOrWhiteSpace(x))?.Trim();

    private static T? PickNullable<T>(IReadOnlyList<IpoCandidate> candidates, Func<IpoCandidate, T?> selector)
        where T : struct => candidates.Select(selector).FirstOrDefault(static x => x.HasValue);

    private static T PickValue<T>(IReadOnlyList<IpoCandidate> candidates, Func<IpoCandidate, T> selector, T empty)
        where T : struct, Enum => candidates.Select(selector).FirstOrDefault(x => !EqualityComparer<T>.Default.Equals(x, empty));
}
