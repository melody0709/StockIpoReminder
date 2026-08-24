using System.Globalization;
using System.IO.Compression;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Microsoft.Data.Sqlite;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Persistence;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Infrastructure.Operations;

public sealed class DiagnosticBundleService
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true,
    };

    private readonly IIpoRepository _repository;
    private readonly DatabaseOptions _databaseOptions;
    private readonly MaintenanceOptions _maintenanceOptions;
    private readonly RuntimeState _runtimeState;
    private readonly TimeProvider _timeProvider;

    public DiagnosticBundleService(
        IIpoRepository repository,
        DatabaseOptions databaseOptions,
        MaintenanceOptions maintenanceOptions,
        RuntimeState runtimeState,
        TimeProvider timeProvider)
    {
        _repository = repository;
        _databaseOptions = databaseOptions;
        _maintenanceOptions = maintenanceOptions;
        _runtimeState = runtimeState;
        _timeProvider = timeProvider;
    }

    public async Task<string> ExportAsync(
        DiagnosticExportOptions? exportOptions = null,
        CancellationToken cancellationToken = default)
    {
        exportOptions ??= new DiagnosticExportOptions();
        await _repository.InitializeAsync(cancellationToken).ConfigureAwait(false);
        Directory.CreateDirectory(_maintenanceOptions.DiagnosticDirectory);
        var generatedAt = _timeProvider.GetUtcNow();
        var finalPath = ResolveBundlePath(generatedAt);
        var temporaryPath = finalPath + $".tmp-{Guid.NewGuid():N}";
        try
        {
            using (var archive = ZipFile.Open(temporaryPath, ZipArchiveMode.Create))
            {
                await WriteJsonAsync(archive, "manifest.json", new
                {
                    generatedAt,
                    applicationVersion = GetApplicationVersion(),
                    framework = RuntimeInformation.FrameworkDescription,
                    os = RuntimeInformation.OSDescription,
                    osArchitecture = RuntimeInformation.OSArchitecture.ToString(),
                    processArchitecture = RuntimeInformation.ProcessArchitecture.ToString(),
                    is64BitProcess = Environment.Is64BitProcess,
                    machineName = "<redacted>",
                    includesRawPayloads = exportOptions.IncludeRawPayloads,
                    includesAnnouncementFiles = exportOptions.IncludeAnnouncementFiles,
                }, cancellationToken).ConfigureAwait(false);

                var settings = await _repository.GetSettingsAsync(cancellationToken).ConfigureAwait(false);
                await WriteJsonAsync(archive, "settings.json", new
                {
                    settings.ShanghaiEnabled,
                    settings.ShenzhenEnabled,
                    settings.BeijingEnabled,
                    settings.ShanghaiBrokerAcceptStart,
                    settings.ShenzhenBrokerAcceptStart,
                    settings.BeijingBrokerAcceptStart,
                    settings.SafetyCutoff,
                    settings.BeijingReservationSupported,
                    settings.SoundEnabled,
                    settings.FlashTaskbar,
                    settings.ToastEnabled,
                    settings.DailyHealthSummaryEnabled,
                    settings.AutoStartEnabled,
                    settings.NormalSyncMinutes,
                    settings.ActiveDaySyncMinutes,
                    settings.NotificationSelfTestCompleted,
                    settings.OnboardingCompleted,
                }, cancellationToken).ConfigureAwait(false);
                await WriteJsonAsync(archive, "runtime.json", _runtimeState.Snapshot, cancellationToken).ConfigureAwait(false);

                var now = ChinaTime.Now(_timeProvider);
                var health = await _repository.GetHealthSummaryAsync(
                    DateOnly.FromDateTime(now.DateTime),
                    now,
                    cancellationToken).ConfigureAwait(false);
                await WriteJsonAsync(archive, "health.json", health, cancellationToken).ConfigureAwait(false);

                var limit = Math.Clamp(exportOptions.RecentRecordLimit, 1, 1000);
                await WriteJsonAsync(
                    archive,
                    "database/schema.json",
                    await ReadRowsAsync(
                        "SELECT version, applied_at FROM schema_migrations ORDER BY version;",
                        limit,
                        cancellationToken).ConfigureAwait(false),
                    cancellationToken).ConfigureAwait(false);
                await WriteJsonAsync(
                    archive,
                    "database/source-health.json",
                    await ReadRowsAsync(
                        "SELECT source, last_attempt_at, last_success_at, last_record_count, schema_fingerprint, consecutive_failures, health_state, last_error FROM source_health ORDER BY source;",
                        limit,
                        cancellationToken).ConfigureAwait(false),
                    cancellationToken).ConfigureAwait(false);
                await WriteJsonAsync(
                    archive,
                    "database/recent-sync-runs.json",
                    await ReadRowsAsync(
                        "SELECT source, started_at, finished_at, success, record_count, error FROM sync_runs ORDER BY finished_at DESC LIMIT $limit;",
                        limit,
                        cancellationToken).ConfigureAwait(false),
                    cancellationToken).ConfigureAwait(false);
                await WriteJsonAsync(
                    archive,
                    "database/recent-reminders.json",
                    await ReadRowsAsync(
                        "SELECT ipo_event_id, scheduled_at, shown_at, reminder_level, delivery_channel, dedupe_key, result FROM reminder_log ORDER BY shown_at DESC LIMIT $limit;",
                        limit,
                        cancellationToken).ConfigureAwait(false),
                    cancellationToken).ConfigureAwait(false);

                await AddLogsAsync(archive, cancellationToken).ConfigureAwait(false);
                if (exportOptions.IncludeRawPayloads)
                {
                    await WriteJsonAsync(
                        archive,
                        "optional/raw-payloads.json",
                        await ReadRowsAsync(
                            "SELECT source, fetched_at, success, record_count, raw_hash, schema_fingerprint, payload, error FROM raw_payloads ORDER BY fetched_at DESC LIMIT $limit;",
                            Math.Min(limit, 20),
                            cancellationToken).ConfigureAwait(false),
                        cancellationToken).ConfigureAwait(false);
                }

                if (exportOptions.IncludeAnnouncementFiles)
                {
                    await AddAnnouncementFilesAsync(archive, Math.Min(limit, 20), cancellationToken).ConfigureAwait(false);
                }
            }

            File.Move(temporaryPath, finalPath);
            return finalPath;
        }
        finally
        {
            if (File.Exists(temporaryPath))
            {
                File.Delete(temporaryPath);
            }
        }
    }

    private async Task<IReadOnlyList<IReadOnlyDictionary<string, object?>>> ReadRowsAsync(
        string sql,
        int limit,
        CancellationToken cancellationToken)
    {
        var rows = new List<IReadOnlyDictionary<string, object?>>();
        await using var connection = new SqliteConnection(_databaseOptions.ConnectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        command.Parameters.AddWithValue("$limit", limit);
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            var row = new Dictionary<string, object?>(reader.FieldCount, StringComparer.Ordinal);
            for (var index = 0; index < reader.FieldCount; index++)
            {
                var value = reader.IsDBNull(index) ? null : reader.GetValue(index);
                row[reader.GetName(index)] = value is string text
                    ? DiagnosticRedactor.Redact(Truncate(text, 100_000))
                    : value;
            }

            rows.Add(row);
        }

        return rows;
    }

    private async Task AddLogsAsync(ZipArchive archive, CancellationToken cancellationToken)
    {
        if (!Directory.Exists(_maintenanceOptions.LogDirectory))
        {
            return;
        }

        var files = Directory.EnumerateFiles(_maintenanceOptions.LogDirectory, "app-*.log", SearchOption.TopDirectoryOnly)
            .Select(static path => new FileInfo(path))
            .OrderByDescending(static file => file.LastWriteTimeUtc)
            .ThenByDescending(static file => file.Name, StringComparer.Ordinal)
            .Take(10)
            .ToArray();
        foreach (var file in files)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var content = await ReadTailAsync(file.FullName, 1_000_000, cancellationToken).ConfigureAwait(false);
            await WriteTextAsync(
                archive,
                $"logs/{file.Name}",
                DiagnosticRedactor.Redact(content),
                cancellationToken).ConfigureAwait(false);
        }
    }

    private async Task AddAnnouncementFilesAsync(ZipArchive archive, int limit, CancellationToken cancellationToken)
    {
        var rows = await ReadRowsAsync(
            "SELECT id, local_path FROM announcement_documents ORDER BY downloaded_at DESC LIMIT $limit;",
            limit,
            cancellationToken).ConfigureAwait(false);
        foreach (var row in rows)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var id = Convert.ToString(row["id"], CultureInfo.InvariantCulture);
            var path = Convert.ToString(row["local_path"], CultureInfo.InvariantCulture);
            if (string.IsNullOrWhiteSpace(id)
                || string.IsNullOrWhiteSpace(path)
                || !File.Exists(path)
                || !IsWithinDirectory(path, _maintenanceOptions.DataRoot))
            {
                continue;
            }

            var extension = Path.GetExtension(path);
            var entry = archive.CreateEntry($"optional/announcements/{SanitizeFileName(id)}{extension}", CompressionLevel.Optimal);
            await using var target = entry.Open();
            await using var source = File.OpenRead(path);
            await source.CopyToAsync(target, cancellationToken).ConfigureAwait(false);
        }
    }

    private static async Task<string> ReadTailAsync(string path, int maximumBytes, CancellationToken cancellationToken)
    {
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete, 81920, FileOptions.Asynchronous);
        var length = Math.Min(stream.Length, maximumBytes);
        stream.Seek(-length, SeekOrigin.End);
        var buffer = new byte[length];
        var total = 0;
        while (total < buffer.Length)
        {
            var read = await stream.ReadAsync(buffer.AsMemory(total), cancellationToken).ConfigureAwait(false);
            if (read == 0)
            {
                break;
            }

            total += read;
        }

        return Encoding.UTF8.GetString(buffer, 0, total);
    }

    private static Task WriteJsonAsync<T>(
        ZipArchive archive,
        string entryName,
        T value,
        CancellationToken cancellationToken) =>
        WriteTextAsync(archive, entryName, DiagnosticRedactor.Redact(JsonSerializer.Serialize(value, JsonOptions)), cancellationToken);

    private static async Task WriteTextAsync(
        ZipArchive archive,
        string entryName,
        string value,
        CancellationToken cancellationToken)
    {
        var entry = archive.CreateEntry(entryName, CompressionLevel.Optimal);
        await using var stream = entry.Open();
        await using var writer = new StreamWriter(stream, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false), leaveOpen: false);
        cancellationToken.ThrowIfCancellationRequested();
        await writer.WriteAsync(value.AsMemory(), cancellationToken).ConfigureAwait(false);
    }

    private string ResolveBundlePath(DateTimeOffset generatedAt)
    {
        var baseName = $"stock-ipo-reminder-diagnostics-{generatedAt.ToUniversalTime():yyyyMMdd-HHmmssfff}";
        for (var index = 0; ; index++)
        {
            var suffix = index == 0 ? string.Empty : $"-{index}";
            var path = Path.Combine(_maintenanceOptions.DiagnosticDirectory, $"{baseName}{suffix}.zip");
            if (!File.Exists(path))
            {
                return path;
            }
        }
    }

    private static string GetApplicationVersion() =>
        Assembly.GetEntryAssembly()?.GetName().Version?.ToString()
        ?? typeof(DiagnosticBundleService).Assembly.GetName().Version?.ToString()
        ?? "unknown";

    private static string Truncate(string value, int maximumCharacters) =>
        value.Length <= maximumCharacters ? value : value[..maximumCharacters] + "<truncated>";

    private static string SanitizeFileName(string value)
    {
        var invalid = Path.GetInvalidFileNameChars();
        return string.Concat(value.Select(character => invalid.Contains(character) ? '_' : character));
    }

    private static bool IsWithinDirectory(string path, string directory)
    {
        var fullPath = Path.GetFullPath(path);
        var fullDirectory = Path.GetFullPath(directory).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)
            + Path.DirectorySeparatorChar;
        return fullPath.StartsWith(fullDirectory, StringComparison.OrdinalIgnoreCase);
    }
}
