using System.Reflection;
using System.Text.Json;

namespace StockIpoReminder.Setup;

internal sealed record InstallationManifest
{
    public required string ProductId { get; init; }

    public required string DisplayName { get; init; }

    public required string Version { get; init; }

    public required string InstanceId { get; init; }

    public required string InstallDirectory { get; init; }

    public required string DataRoot { get; init; }

    public required string RegistryKeyName { get; init; }

    public required string StartMenuShortcutPath { get; init; }

    public required string AutoStartTaskName { get; init; }

    public required DateTimeOffset InstalledAtUtc { get; init; }
}

internal sealed record DataDirectoryMarker
{
    public required string ProductId { get; init; }

    public required string InstanceId { get; init; }

    public required string DataRoot { get; init; }

    public required DateTimeOffset CreatedAtUtc { get; init; }
}

internal sealed record SetupResult(
    bool Success,
    int ExitCode,
    string Operation,
    string Message,
    string? InstallDirectory = null,
    string? DataRoot = null,
    string? BackupPath = null,
    bool? DataPreserved = null);

internal static class SetupJson
{
    public static readonly JsonSerializerOptions Options = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true,
    };

    public static string ProductVersion => Assembly.GetExecutingAssembly().GetName().Version?.ToString(3) ?? "0.1.1";

    public static async Task WriteAtomicAsync<T>(string path, T value, CancellationToken cancellationToken = default)
    {
        var directory = Path.GetDirectoryName(path);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        var temporaryPath = path + $".{Guid.NewGuid():N}.tmp";
        await using (var stream = new FileStream(
            temporaryPath,
            FileMode.CreateNew,
            FileAccess.Write,
            FileShare.None,
            bufferSize: 16 * 1024,
            FileOptions.Asynchronous | FileOptions.WriteThrough))
        {
            await JsonSerializer.SerializeAsync(stream, value, Options, cancellationToken).ConfigureAwait(false);
            await stream.FlushAsync(cancellationToken).ConfigureAwait(false);
        }

        File.Move(temporaryPath, path, overwrite: true);
    }

    public static async Task<T> ReadAsync<T>(string path, CancellationToken cancellationToken = default)
    {
        await using var stream = new FileStream(
            path,
            FileMode.Open,
            FileAccess.Read,
            FileShare.Read,
            bufferSize: 16 * 1024,
            FileOptions.Asynchronous | FileOptions.SequentialScan);
        return await JsonSerializer.DeserializeAsync<T>(stream, Options, cancellationToken).ConfigureAwait(false)
            ?? throw new InvalidDataException($"文件内容为空或格式无效：{path}");
    }
}
