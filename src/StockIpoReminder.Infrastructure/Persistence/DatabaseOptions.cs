namespace StockIpoReminder.Infrastructure.Persistence;

public sealed record DatabaseOptions
{
    public required string DatabasePath { get; init; }
    public bool Pooling { get; init; } = true;

    public string ConnectionString => new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
    {
        DataSource = DatabasePath,
        Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadWriteCreate,
        Cache = Microsoft.Data.Sqlite.SqliteCacheMode.Shared,
        Pooling = Pooling,
        DefaultTimeout = 10,
    }.ToString();
}
