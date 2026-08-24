using System.Globalization;
using System.Text;

namespace StockIpoReminder.Infrastructure.Operations;

public sealed class RollingFileLogWriter : IDisposable
{
    private readonly string _directory;
    private readonly long _maxFileBytes;
    private readonly TimeSpan _retention;
    private readonly TimeProvider _timeProvider;
    private readonly object _gate = new();
    private DateOnly? _lastCleanupDate;

    public RollingFileLogWriter(
        string directory,
        long maxFileBytes = 5 * 1024 * 1024,
        TimeSpan? retention = null,
        TimeProvider? timeProvider = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(directory);
        ArgumentOutOfRangeException.ThrowIfLessThan(maxFileBytes, 1024);
        _directory = directory;
        _maxFileBytes = maxFileBytes;
        _retention = retention ?? TimeSpan.FromDays(90);
        _timeProvider = timeProvider ?? TimeProvider.System;
        Directory.CreateDirectory(_directory);
    }

    public void Write(string value)
    {
        var redacted = DiagnosticRedactor.Redact(value);
        var byteCount = Encoding.UTF8.GetByteCount(redacted);
        lock (_gate)
        {
            var localNow = _timeProvider.GetLocalNow();
            var date = DateOnly.FromDateTime(localNow.DateTime);
            if (_lastCleanupDate != date)
            {
                DeleteExpiredLogs(localNow);
                _lastCleanupDate = date;
            }

            var path = ResolvePath(date, byteCount);
            File.AppendAllText(path, redacted, Encoding.UTF8);
        }
    }

    public void Dispose()
    {
    }

    private string ResolvePath(DateOnly date, int incomingBytes)
    {
        var stamp = date.ToString("yyyyMMdd", CultureInfo.InvariantCulture);
        for (var index = 0; ; index++)
        {
            var suffix = index == 0 ? string.Empty : $".{index:000}";
            var path = Path.Combine(_directory, $"app-{stamp}{suffix}.log");
            if (!File.Exists(path) || new FileInfo(path).Length + incomingBytes <= _maxFileBytes)
            {
                return path;
            }
        }
    }

    private void DeleteExpiredLogs(DateTimeOffset now)
    {
        var cutoff = now.UtcDateTime - _retention;
        foreach (var path in Directory.EnumerateFiles(_directory, "app-*.log", SearchOption.TopDirectoryOnly))
        {
            try
            {
                if (File.GetLastWriteTimeUtc(path) < cutoff)
                {
                    File.Delete(path);
                }
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
        }
    }
}
