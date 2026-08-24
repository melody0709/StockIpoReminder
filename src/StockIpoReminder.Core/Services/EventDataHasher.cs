using System.Globalization;
using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Core.Services;

public static class EventDataHasher
{
    public static string Compute(IpoEvent ipoEvent)
    {
        var sessions = string.Join(';', ipoEvent.Sessions.Select(static x =>
            $"{x.SessionNumber}:{x.OfficialStart:HH:mm}-{x.OfficialEnd:HH:mm}:{x.SafetyCutoff:HH:mm}"));
        var canonical = string.Join('|',
            ipoEvent.Id,
            ipoEvent.EventVersion.ToString(CultureInfo.InvariantCulture),
            ipoEvent.ApplyCode,
            ipoEvent.ApplyDate?.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            ipoEvent.IssuePrice?.ToString(CultureInfo.InvariantCulture),
            ipoEvent.LotSize?.ToString(CultureInfo.InvariantCulture),
            ipoEvent.MaxApplyQuantity?.ToString(CultureInfo.InvariantCulture),
            ipoEvent.Status.ToString(),
            sessions);
        return ValueNormalizer.Sha256(canonical);
    }
}
