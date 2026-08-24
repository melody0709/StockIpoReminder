using System.Net;
using StockIpoReminder.Core.Abstractions;

namespace StockIpoReminder.Infrastructure.Collectors;

internal sealed class SourceHttpException : HttpRequestException, IRetryAfterError
{
    public SourceHttpException(HttpStatusCode statusCode, string? reasonPhrase, TimeSpan? retryAfter)
        : base($"HTTP {(int)statusCode} {reasonPhrase}", inner: null, statusCode)
    {
        RetryAfter = retryAfter;
    }

    public TimeSpan? RetryAfter { get; }
}

internal static class HttpResponseGuard
{
    public static void EnsureSuccess(HttpResponseMessage response, DateTimeOffset now)
    {
        if (response.IsSuccessStatusCode)
        {
            return;
        }

        TimeSpan? retryAfter = response.Headers.RetryAfter?.Delta;
        if (response.Headers.RetryAfter?.Date is { } retryDate)
        {
            var dateDelay = retryDate - now;
            if (dateDelay > TimeSpan.Zero)
            {
                retryAfter = dateDelay;
            }
        }

        throw new SourceHttpException(response.StatusCode, response.ReasonPhrase, retryAfter);
    }
}
