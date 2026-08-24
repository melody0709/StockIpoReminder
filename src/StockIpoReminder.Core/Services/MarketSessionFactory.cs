using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Core.Services;

public static class MarketSessionFactory
{
    public static IReadOnlyList<SubscriptionSession> CreateDefault(Exchange exchange, AppSettings settings)
    {
        var (start, brokerStart, fundingMode, timeSensitive) = exchange switch
        {
            Exchange.Shanghai => (new TimeOnly(9, 30), settings.ShanghaiBrokerAcceptStart, FundingMode.MarketValue, false),
            Exchange.Shenzhen => (new TimeOnly(9, 15), settings.ShenzhenBrokerAcceptStart, FundingMode.MarketValue, false),
            Exchange.Beijing => (new TimeOnly(9, 15), settings.BeijingBrokerAcceptStart, FundingMode.FullCash, true),
            _ => (new TimeOnly(9, 30), new TimeOnly(9, 30), FundingMode.MarketValue, false),
        };

        var cutoff = settings.SafetyCutoff > new TimeOnly(15, 0)
            ? new TimeOnly(15, 0)
            : settings.SafetyCutoff;

        return
        [
            new SubscriptionSession
            {
                SessionNumber = 1,
                OfficialStart = start,
                OfficialEnd = new TimeOnly(11, 30),
                BrokerAcceptStart = brokerStart,
                FundingMode = fundingMode,
                AllocationTimeSensitive = timeSensitive,
            },
            new SubscriptionSession
            {
                SessionNumber = 2,
                OfficialStart = new TimeOnly(13, 0),
                OfficialEnd = new TimeOnly(15, 0),
                SafetyCutoff = cutoff,
                FundingMode = fundingMode,
                AllocationTimeSensitive = timeSensitive,
            },
        ];
    }
}
