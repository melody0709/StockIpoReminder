using System.IO;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Infrastructure.Operations;

namespace StockIpoReminder.App;

public sealed class FileLoggerProvider : ILoggerProvider
{
    private readonly RollingFileLogWriter _writer;

    public FileLoggerProvider(string directory)
    {
        Directory.CreateDirectory(directory);
        _writer = new RollingFileLogWriter(directory);
    }

    public ILogger CreateLogger(string categoryName) => new FileLogger(this, categoryName);
    public void Dispose() => _writer.Dispose();

    private sealed class FileLogger : ILogger
    {
        private readonly FileLoggerProvider _provider;
        private readonly string _category;

        public FileLogger(FileLoggerProvider provider, string category)
        {
            _provider = provider;
            _category = category;
        }

        public IDisposable? BeginScope<TState>(TState state) where TState : notnull => null;
        public bool IsEnabled(LogLevel logLevel) => logLevel >= LogLevel.Information;

        public void Log<TState>(
            LogLevel logLevel,
            EventId eventId,
            TState state,
            Exception? exception,
            Func<TState, Exception?, string> formatter)
        {
            if (!IsEnabled(logLevel))
            {
                return;
            }

            var line = $"{DateTimeOffset.Now:O} [{logLevel}] {_category}: {formatter(state, exception)}{Environment.NewLine}";
            if (exception is not null)
            {
                line += exception + Environment.NewLine;
            }

            _provider._writer.Write(line);
        }
    }
}
