using System.Globalization;
using System.IO.Compression;
using System.Text;
using Microsoft.Data.Sqlite;
using Microsoft.Extensions.Logging.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class OperationsTests
{
    [TestMethod]
    public void Diagnostic_Redactor_Removes_Queries_Headers_And_Secret_Fields()
    {
        const string input = """
            GET https://example.invalid/path?token=abc&user=42
            Authorization: Bearer super-secret
            Cookie: session=private
            {"password":"pass-123","api_key":"key-456","safe":"visible"}
            """;

        var result = DiagnosticRedactor.Redact(input);

        StringAssert.Contains(result, "https://example.invalid/path?<redacted>");
        StringAssert.Contains(result, "Authorization: <redacted>");
        StringAssert.Contains(result, "Cookie: <redacted>");
        StringAssert.Contains(result, "\"password\":\"<redacted>\"");
        StringAssert.Contains(result, "\"api_key\":\"<redacted>\"");
        StringAssert.Contains(result, "\"safe\":\"visible\"");
        Assert.IsFalse(result.Contains("super-secret", StringComparison.Ordinal));
        Assert.IsFalse(result.Contains("pass-123", StringComparison.Ordinal));
        Assert.IsFalse(result.Contains("key-456", StringComparison.Ordinal));
    }

    [TestMethod]
    public void Rolling_Log_Rotates_Redacts_And_Removes_Expired_Files()
    {
        var directory = Directory.CreateTempSubdirectory("stock-ipo-logs-");
        try
        {
            var expired = Path.Combine(directory.FullName, "app-20200101.log");
            File.WriteAllText(expired, "expired");
            File.SetLastWriteTimeUtc(expired, DateTime.UtcNow.AddDays(-100));

            using var writer = new RollingFileLogWriter(
                directory.FullName,
                maxFileBytes: 1024,
                retention: TimeSpan.FromDays(90));
            var padding = new string('x', 700);
            writer.Write($"{padding}{Environment.NewLine}");
            writer.Write($"{padding}{Environment.NewLine}");
            writer.Write($"Authorization: Bearer log-secret{Environment.NewLine}");
            writer.Write($"GET https://example.invalid/?token=query-secret{Environment.NewLine}");

            var files = Directory.EnumerateFiles(directory.FullName, "app-*.log").ToArray();
            Assert.HasCount(2, files);
            Assert.IsFalse(File.Exists(expired));
            var content = string.Concat(files.Select(File.ReadAllText));
            Assert.IsFalse(content.Contains("log-secret", StringComparison.Ordinal));
            Assert.IsFalse(content.Contains("query-secret", StringComparison.Ordinal));
            StringAssert.Contains(content, "<redacted>");
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }

    [TestMethod]
    public async Task Maintenance_Cleans_Expired_Unprotected_Data_And_Creates_Verified_Backups()
    {
        var now = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero);
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var dataRoot = Path.GetDirectoryName(context.Options.DatabasePath)!;
        var options = CreateOptions(dataRoot) with { BackupRetentionCount = 2, DeleteBatchSize = 10 };
        var active = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:601201",
            new DateOnly(2026, 8, 24),
            lifecycle: IpoLifecycleStatus.ActiveUnconfirmed,
            status: IssueStatus.Active));
        var completed = await context.Repository.UpsertEventAsync(RepositoryTestContext.Reconciled(
            "shanghai:601202",
            new DateOnly(2024, 1, 2),
            lifecycle: IpoLifecycleStatus.Acknowledged,
            status: IssueStatus.Completed));
        var oldOperational = now.AddDays(-100);
        await context.Repository.SaveCollectorResultAsync(Result("referenced", "fixture-hash", oldOperational));
        await context.Repository.SaveCollectorResultAsync(Result("unreferenced", "expired-hash", oldOperational));

        var announcementDirectory = Directory.CreateDirectory(Path.Combine(dataRoot, "announcements"));
        var announcementPath = Path.Combine(announcementDirectory.FullName, "old.pdf");
        await File.WriteAllTextAsync(announcementPath, "old announcement");
        await context.Repository.SaveAnnouncementAsync(new AnnouncementDocument
        {
            Id = "old-announcement",
            IpoEventId = completed.Event.Id,
            Reference = new AnnouncementReference
            {
                Provider = "fixture",
                AnnouncementId = "old",
                Title = "旧公告",
                Url = new Uri("https://example.invalid/old.pdf?token=secret"),
            },
            LocalPath = announcementPath,
            FileHash = "old-file-hash",
            ExtractionStatus = ExtractionStatus.Extracted,
            DownloadedAt = now.AddDays(-800),
        });
        await InsertOperationalHistoryAsync(context, active.Event.Id, completed.Event.Id, now);

        var service = new OperationalMaintenanceService(
            context.Repository,
            context.Options,
            options,
            context.TimeProvider,
            NullLogger<OperationalMaintenanceService>.Instance);
        var result = await service.RunOnceAsync();

        Assert.IsNotNull(result.BackupPath);
        Assert.IsTrue(File.Exists(result.BackupPath));
        Assert.AreEqual(1L, await ScalarAsync(context, "SELECT COUNT(*) FROM raw_payloads WHERE raw_hash = 'fixture-hash';"));
        Assert.AreEqual(0L, await ScalarAsync(context, "SELECT COUNT(*) FROM raw_payloads WHERE raw_hash = 'expired-hash';"));
        Assert.AreEqual(1L, await ScalarAsync(context, $"SELECT COUNT(*) FROM reminder_log WHERE ipo_event_id = '{active.Event.Id}';"));
        Assert.AreEqual(0L, await ScalarAsync(context, $"SELECT COUNT(*) FROM reminder_log WHERE ipo_event_id = '{completed.Event.Id}';"));
        Assert.AreEqual(1L, await ScalarAsync(context, $"SELECT COUNT(*) FROM acknowledgements WHERE ipo_event_id = '{active.Event.Id}';"));
        Assert.AreEqual(0L, await ScalarAsync(context, $"SELECT COUNT(*) FROM acknowledgements WHERE ipo_event_id = '{completed.Event.Id}';"));
        Assert.AreEqual(0L, await ScalarAsync(context, "SELECT COUNT(*) FROM announcement_documents WHERE id = 'old-announcement';"));
        Assert.IsFalse(File.Exists(announcementPath));
        await AssertIntegrityAsync(result.BackupPath);

        await service.CreateBackupAsync(now.AddSeconds(1));
        await service.CreateBackupAsync(now.AddSeconds(2));
        Assert.HasCount(2, Directory.EnumerateFiles(options.BackupDirectory, "*.db").ToArray());
    }

    [TestMethod]
    public async Task Diagnostic_Bundle_Is_Redacted_And_Excludes_Raw_Data_By_Default()
    {
        var now = new DateTimeOffset(2026, 8, 24, 2, 0, 0, TimeSpan.Zero);
        await using var context = await RepositoryTestContext.CreateAsync(now);
        var dataRoot = Path.GetDirectoryName(context.Options.DatabasePath)!;
        var options = CreateOptions(dataRoot);
        Directory.CreateDirectory(options.LogDirectory);
        await File.WriteAllTextAsync(
            Path.Combine(options.LogDirectory, "app-20260824.log"),
            "Authorization: Bearer log-private https://example.invalid/path?token=url-private");
        await context.Repository.SaveCollectorResultAsync(new CollectorResult
        {
            Source = "diagnostic-source",
            Success = false,
            StartedAt = now,
            FinishedAt = now,
            RawPayload = "{\"token\":\"payload-private\"}",
            RawHash = "diagnostic-hash",
            Error = "Cookie: session=cookie-private",
        });
        var runtime = new RuntimeState();
        runtime.Update(snapshot => snapshot with
        {
            StatusText = "https://example.invalid/status?token=status-private",
            LastError = "Authorization: Bearer runtime-private",
        });
        var service = new DiagnosticBundleService(
            context.Repository,
            context.Options,
            options,
            runtime,
            context.TimeProvider);

        var path = await service.ExportAsync();

        using var archive = ZipFile.OpenRead(path);
        Assert.IsNotNull(archive.GetEntry("manifest.json"));
        Assert.IsNotNull(archive.GetEntry("settings.json"));
        Assert.IsNotNull(archive.GetEntry("runtime.json"));
        Assert.IsNull(archive.GetEntry("optional/raw-payloads.json"));
        Assert.IsFalse(archive.Entries.Any(entry => entry.FullName.StartsWith("optional/announcements/", StringComparison.Ordinal)));
        var text = await ReadTextEntriesAsync(archive);
        foreach (var secret in new[] { "log-private", "url-private", "payload-private", "cookie-private", "status-private", "runtime-private" })
        {
            Assert.IsFalse(text.Contains(secret, StringComparison.Ordinal), $"Diagnostic bundle leaked {secret}");
        }
        StringAssert.Contains(text, "<redacted>");
    }

    private static MaintenanceOptions CreateOptions(string dataRoot) => new()
    {
        DataRoot = dataRoot,
        LogDirectory = Path.Combine(dataRoot, "logs"),
        BackupDirectory = Path.Combine(dataRoot, "backups"),
        DiagnosticDirectory = Path.Combine(dataRoot, "diagnostics"),
        InitialDelay = TimeSpan.Zero,
        MaintenanceInterval = TimeSpan.FromDays(1),
    };

    private static CollectorResult Result(string source, string hash, DateTimeOffset timestamp) => new()
    {
        Source = source,
        Success = true,
        StartedAt = timestamp,
        FinishedAt = timestamp,
        RecordCount = 0,
        RawPayload = "[]",
        RawHash = hash,
        SchemaFingerprint = "schema",
    };

    private static async Task InsertOperationalHistoryAsync(
        RepositoryTestContext context,
        string activeId,
        string completedId,
        DateTimeOffset now)
    {
        await using var connection = await context.OpenConnectionAsync();
        await using var command = connection.CreateCommand();
        command.CommandText = """
            INSERT INTO reminder_log(ipo_event_id, scheduled_at, shown_at, reminder_level, delivery_channel, dedupe_key, result)
            VALUES($active, $oldOperational, $oldOperational, 30, 'test', 'active-log', 'shown'),
                  ($completed, $oldOperational, $oldOperational, 30, 'test', 'completed-log', 'shown');

            INSERT INTO acknowledgements(ipo_event_id, event_version, confirmed_at, confirmed_data_hash, revoked_at)
            VALUES($active, 1, $oldAudit, 'active-ack', NULL),
                  ($completed, 1, $oldAudit, 'completed-ack', NULL);

            INSERT INTO manual_overrides(ipo_event_id, event_version, field_name, override_value, reason, created_at, revoked_at)
            VALUES($active, 1, 'IssuePrice', '10.00', 'active', $oldAudit, $oldAudit),
                  ($completed, 1, 'IssuePrice', '10.00', 'completed', $oldAudit, $oldAudit);
            """;
        command.Parameters.AddWithValue("$active", activeId);
        command.Parameters.AddWithValue("$completed", completedId);
        command.Parameters.AddWithValue("$oldOperational", now.AddDays(-100).ToString("O", CultureInfo.InvariantCulture));
        command.Parameters.AddWithValue("$oldAudit", now.AddDays(-800).ToString("O", CultureInfo.InvariantCulture));
        await command.ExecuteNonQueryAsync();
    }

    private static async Task<long> ScalarAsync(RepositoryTestContext context, string sql)
    {
        await using var connection = await context.OpenConnectionAsync();
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        return Convert.ToInt64(await command.ExecuteScalarAsync(), CultureInfo.InvariantCulture);
    }

    private static async Task AssertIntegrityAsync(string path)
    {
        await using var connection = new SqliteConnection(new SqliteConnectionStringBuilder
        {
            DataSource = path,
            Mode = SqliteOpenMode.ReadOnly,
            Pooling = false,
        }.ToString());
        await connection.OpenAsync();
        await using var command = connection.CreateCommand();
        command.CommandText = "PRAGMA integrity_check;";
        Assert.AreEqual("ok", Convert.ToString(await command.ExecuteScalarAsync(), CultureInfo.InvariantCulture));
    }

    private static async Task<string> ReadTextEntriesAsync(ZipArchive archive)
    {
        var builder = new StringBuilder();
        foreach (var entry in archive.Entries.Where(static item =>
                     item.FullName.EndsWith(".json", StringComparison.OrdinalIgnoreCase)
                     || item.FullName.EndsWith(".log", StringComparison.OrdinalIgnoreCase)))
        {
            await using var stream = entry.Open();
            using var reader = new StreamReader(stream, Encoding.UTF8);
            builder.AppendLine(await reader.ReadToEndAsync());
        }

        return builder.ToString();
    }
}
