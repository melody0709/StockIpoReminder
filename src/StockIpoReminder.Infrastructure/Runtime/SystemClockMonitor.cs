using System.Globalization;
using System.Net.Http.Headers;
using System.Threading.Channels;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using StockIpoReminder.Core.Domain;

namespace StockIpoReminder.Infrastructure.Runtime;

public sealed record SystemClockOptions
{
    public IReadOnlyList<Uri> Endpoints { get; init; } =
    [
        new("https://www.microsoft.com/"),
        new("https://www.cloudflare.com/"),
    ];

    public TimeSpan WarningThreshold { get; init; } = TimeSpan.FromMinutes(2);
    public TimeSpan FailureThreshold { get; init; } = TimeSpan.FromMinutes(5);
    public TimeSpan CheckInterval { get; init; } = TimeSpan.FromHours(6);
}

public interface ISystemClockCheckTrigger
{
    void RequestCheck(string reason);
}

public sealed class SystemClockMonitor : BackgroundService, ISystemClockCheckTrigger
{
    private readonly Channel<string> _requests = Channel.CreateBounded<string>(new BoundedChannelOptions(1)
    {
        SingleReader = true,
        SingleWriter = false,
        FullMode = BoundedChannelFullMode.DropOldest,
    });
    private readonly HttpClient _httpClient;
    private readonly SystemClockOptions _options;
    private readonly RuntimeState _runtimeState;
    private readonly TimeProvider _timeProvider;
    private readonly ILogger<SystemClockMonitor> _logger;

    public SystemClockMonitor(
        HttpClient httpClient,
        SystemClockOptions options,
        RuntimeState runtimeState,
        TimeProvider timeProvider,
        ILogger<SystemClockMonitor> logger)
    {
        _httpClient = httpClient;
        _options = options;
        _runtimeState = runtimeState;
        _timeProvider = timeProvider;
        _logger = logger;
    }

    public void RequestCheck(string reason) => _requests.Writer.TryWrite(reason);

    public async Task<SystemClockSnapshot> CheckAsync(
        string reason,
        CancellationToken cancellationToken = default)
    {
        var samples = await Task.WhenAll(_options.Endpoints.Select(endpoint =>
            ProbeAsync(endpoint, cancellationToken))).ConfigureAwait(false);
        var valid = samples
            .Where(static sample => sample.Offset is not null)
            .Select(static sample => sample.Offset!.Value.Ticks)
            .Order()
            .ToArray();

        var checkedAt = _timeProvider.GetUtcNow();
        TimeSpan? estimatedOffset = valid.Length == 0
            ? null
            : TimeSpan.FromTicks(valid.Length % 2 == 1
                ? valid[valid.Length / 2]
                : (valid[(valid.Length / 2) - 1] / 2) + (valid[valid.Length / 2] / 2));
        var state = GetState(estimatedOffset, valid.Length);
        var snapshot = new SystemClockSnapshot
        {
            State = state,
            CheckedAt = checkedAt,
            EstimatedOffset = estimatedOffset,
            ValidSampleCount = valid.Length,
            ExpectedSampleCount = _options.Endpoints.Count,
            Message = GetMessage(state, estimatedOffset, valid.Length, _options.Endpoints.Count, reason),
            Samples = samples,
        };
        _runtimeState.Update(current => current with { Clock = snapshot });
        if (state is HealthState.Warning or HealthState.Failed)
        {
            _logger.LogWarning("系统时间检查结果：{ClockMessage}", snapshot.Message);
        }

        return snapshot;
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        RequestCheck("程序启动");
        while (!stoppingToken.IsCancellationRequested)
        {
            try
            {
                var read = _requests.Reader.ReadAsync(stoppingToken).AsTask();
                var delay = Task.Delay(_options.CheckInterval, stoppingToken);
                var completed = await Task.WhenAny(read, delay).ConfigureAwait(false);
                var reason = completed == read ? await read.ConfigureAwait(false) : "周期检查";
                await CheckAsync(reason, stoppingToken).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                break;
            }
            catch (Exception ex)
            {
                _logger.LogWarning(ex, "系统时间检查失败");
            }
        }
    }

    private async Task<SystemClockSample> ProbeAsync(Uri endpoint, CancellationToken cancellationToken)
    {
        var start = _timeProvider.GetUtcNow();
        try
        {
            var builder = new UriBuilder(endpoint);
            var cacheBuster = $"clock_probe={start.ToUnixTimeMilliseconds().ToString(CultureInfo.InvariantCulture)}";
            builder.Query = string.IsNullOrWhiteSpace(builder.Query)
                ? cacheBuster
                : $"{builder.Query.TrimStart('?')}&{cacheBuster}";
            using var request = new HttpRequestMessage(HttpMethod.Get, builder.Uri);
            request.Headers.CacheControl = new CacheControlHeaderValue { NoCache = true, NoStore = true };
            using var response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                cancellationToken).ConfigureAwait(false);
            var end = _timeProvider.GetUtcNow();
            if (response.Headers.Date is not { } serverTime)
            {
                return new SystemClockSample
                {
                    Source = endpoint.Host,
                    Error = $"HTTP {(int)response.StatusCode} 未提供 Date 响应头",
                };
            }

            var midpoint = start + TimeSpan.FromTicks((end - start).Ticks / 2);
            return new SystemClockSample
            {
                Source = endpoint.Host,
                ServerTime = serverTime,
                Offset = serverTime - midpoint,
            };
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            return new SystemClockSample
            {
                Source = endpoint.Host,
                Error = ex.GetType().Name,
            };
        }
    }

    private HealthState GetState(TimeSpan? estimatedOffset, int validSampleCount)
    {
        if (validSampleCount == 0 || estimatedOffset is null)
        {
            return HealthState.Unknown;
        }

        if (validSampleCount < 2)
        {
            return HealthState.Warning;
        }

        var absolute = estimatedOffset.Value.Duration();
        return absolute > _options.FailureThreshold
            ? HealthState.Failed
            : absolute > _options.WarningThreshold
                ? HealthState.Warning
                : HealthState.Healthy;
    }

    private static string GetMessage(
        HealthState state,
        TimeSpan? estimatedOffset,
        int valid,
        int expected,
        string reason)
    {
        if (estimatedOffset is null)
        {
            return $"无法取得独立网络时间样本（0/{expected}，{reason}），未据此修改任务状态";
        }

        var sign = estimatedOffset.Value < TimeSpan.Zero ? "-" : "+";
        var seconds = estimatedOffset.Value.Duration().TotalSeconds;
        var prefix = state switch
        {
            HealthState.Healthy => "系统时间正常",
            HealthState.Warning when valid < 2 => "系统时间样本不足",
            HealthState.Warning => "系统时间可能有偏差",
            HealthState.Failed => "系统时间偏差过大",
            _ => "系统时间状态未知",
        };
        return $"{prefix}：估算偏差 {sign}{seconds:0} 秒，有效样本 {valid}/{expected}（{reason}）";
    }
}
