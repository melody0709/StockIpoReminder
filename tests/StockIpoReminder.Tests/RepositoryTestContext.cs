using Microsoft.Data.Sqlite;
using Microsoft.Extensions.Logging.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Persistence;

namespace StockIpoReminder.Tests;

internal sealed class RepositoryTestContext : IAsyncDisposable
{
    private readonly DirectoryInfo _directory;

    private RepositoryTestContext(DirectoryInfo directory, MutableTimeProvider timeProvider)
    {
        _directory = directory;
        TimeProvider = timeProvider;
        Options = new DatabaseOptions
        {
            DatabasePath = Path.Combine(directory.FullName, "test.db"),
            Pooling = false,
        };
        Repository = new SqliteIpoRepository(Options, NullLogger<SqliteIpoRepository>.Instance, TimeProvider);
    }

    public DatabaseOptions Options { get; }
    public MutableTimeProvider TimeProvider { get; }
    public SqliteIpoRepository Repository { get; }

    public static async Task<RepositoryTestContext> CreateAsync(DateTimeOffset? now = null)
    {
        var directory = Directory.CreateTempSubdirectory("stock-ipo-reminder-tests-");
        var context = new RepositoryTestContext(
            directory,
            new MutableTimeProvider(now ?? new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero)));
        await context.Repository.InitializeAsync().ConfigureAwait(false);
        return context;
    }

    public async Task<SqliteConnection> OpenConnectionAsync()
    {
        var connection = new SqliteConnection(Options.ConnectionString);
        await connection.OpenAsync().ConfigureAwait(false);
        return connection;
    }

    public async ValueTask DisposeAsync()
    {
        await Task.Yield();
        if (_directory.Exists)
        {
            _directory.Delete(recursive: true);
        }
    }

    public static ReconciledIpoEvent Reconciled(
        string id,
        DateOnly applyDate,
        string applyCode = "730001",
        decimal? issuePrice = 10.50m,
        IpoLifecycleStatus lifecycle = IpoLifecycleStatus.Scheduled,
        IssueStatus status = IssueStatus.Upcoming,
        IReadOnlyList<SubscriptionSession>? sessions = null) => new()
    {
        Event = new IpoEvent
        {
            Id = id,
            Exchange = Exchange.Shanghai,
            Board = Board.Main,
            SecurityCode = id.Split(':')[^1],
            ApplyCode = applyCode,
            LegacyCode = "430001",
            Name = $"测试股份{id}",
            ApplyDate = applyDate,
            IssuePrice = issuePrice,
            LotSize = 500,
            MaxApplyQuantity = 15000,
            RequiredMarketValue = 150000m,
            BallotDate = applyDate.AddDays(2),
            PaymentDate = applyDate.AddDays(2),
            Status = status,
            LifecycleStatus = lifecycle,
            EventVersion = 1,
            AnnouncementUrl = "https://example.invalid/announcement.pdf",
            DataQualityStatus = DataQualityStatus.MultiSourceVerified,
            FirstSeenAt = new DateTimeOffset(2026, 8, 24, 1, 0, 0, TimeSpan.Zero),
            UpdatedAt = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero),
            Sessions = sessions ?? [],
        },
        FieldSources =
        [
            new SourceFieldValue
            {
                FieldName = nameof(IpoEvent.ApplyCode),
                RawValue = applyCode,
                NormalizedValue = applyCode,
                Source = "fixture",
                Priority = 100,
                FetchedAt = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero),
                RawHash = "fixture-hash",
            },
        ],
    };

    public sealed class MutableTimeProvider : TimeProvider
    {
        private DateTimeOffset _utcNow;

        public MutableTimeProvider(DateTimeOffset utcNow)
        {
            _utcNow = utcNow.ToUniversalTime();
        }

        public override DateTimeOffset GetUtcNow() => _utcNow;

        public void SetUtcNow(DateTimeOffset value) => _utcNow = value.ToUniversalTime();
    }
}
