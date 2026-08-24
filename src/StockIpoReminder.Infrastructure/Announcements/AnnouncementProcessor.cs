using System.Net.Http.Headers;
using System.Text;
using System.Text.RegularExpressions;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Collectors;
using StockIpoReminder.Infrastructure.Runtime;
using UglyToad.PdfPig;

namespace StockIpoReminder.Infrastructure.Announcements;

public sealed partial class AnnouncementProcessor : IAnnouncementProcessor, IDisposable
{
    private readonly HttpClient _httpClient;
    private readonly AnnouncementOptions _options;
    private readonly AnnouncementFieldParser _parser;
    private readonly TimeProvider _timeProvider;
    private readonly SemaphoreSlim _sessionGate = new(1, 1);
    private readonly HashSet<string> _initializedSessions = new(StringComparer.OrdinalIgnoreCase);

    public AnnouncementProcessor(
        HttpClient httpClient,
        AnnouncementOptions options,
        AnnouncementFieldParser parser,
        TimeProvider timeProvider)
    {
        _httpClient = httpClient;
        _options = options;
        _parser = parser;
        _timeProvider = timeProvider;
    }

    public async Task<AnnouncementDocument> DownloadAndParseAsync(
        IpoEvent ipoEvent,
        AnnouncementReference announcement,
        CancellationToken cancellationToken)
    {
        OutboundNetworkPolicy.EnsureAllowedAnnouncementHttps(announcement.Url);
        var session = SessionFor(announcement.Url);
        if (session is not null)
        {
            await EnsureSessionAsync(session.Value, cancellationToken).ConfigureAwait(false);
        }

        using var request = new HttpRequestMessage(HttpMethod.Get, announcement.Url);
        if (session is not null)
        {
            request.Headers.Referrer = session.Value.Referrer;
        }

        using var response = await _httpClient.SendAsync(
            request,
            HttpCompletionOption.ResponseHeadersRead,
            cancellationToken).ConfigureAwait(false);
        HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
        var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
        if (bytes.Length == 0)
        {
            throw new InvalidDataException("公告文件为空。" );
        }

        var hasPdfSignature = HasPdfSignature(bytes);
        var claimsPdf = ClaimsPdf(response.Content.Headers.ContentType, announcement.Url);
        if (claimsPdf && !hasPdfSignature)
        {
            var mediaType = response.Content.Headers.ContentType?.MediaType ?? "<missing>";
            throw new InvalidDataException(
                $"公告下载结果不是有效 PDF：host={announcement.Url.Host}；contentType={mediaType}；length={bytes.Length}；缺少 %PDF 文件签名。" );
        }

        var isHtml = !hasPdfSignature && IsHtml(response.Content.Headers.ContentType, bytes);
        if (!hasPdfSignature && !isHtml)
        {
            var mediaType = response.Content.Headers.ContentType?.MediaType ?? "<missing>";
            throw new InvalidDataException(
                $"公告下载结果既不是 PDF 也不是可识别 HTML：host={announcement.Url.Host}；contentType={mediaType}；length={bytes.Length}。" );
        }

        var hash = ValueNormalizer.Sha256(bytes);
        var extension = hasPdfSignature ? ".pdf" : ".html";
        var directory = Path.Combine(_options.StorageDirectory, Sanitize(ipoEvent.Id));
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, $"{Sanitize(announcement.AnnouncementId)}-{hash[..12]}{extension}");
        await File.WriteAllBytesAsync(path, bytes, cancellationToken).ConfigureAwait(false);

        string? text = null;
        ExtractionStatus status;
        try
        {
            text = extension == ".pdf" ? ExtractPdfText(bytes) : ExtractHtmlText(bytes);
            status = string.IsNullOrWhiteSpace(text) ? ExtractionStatus.Unsupported : ExtractionStatus.Extracted;
        }
        catch
        {
            status = ExtractionStatus.Failed;
        }

        var fields = text is null ? [] : _parser.Parse(text, announcement.Title);
        if (status == ExtractionStatus.Extracted && fields.Count == 0)
        {
            status = ExtractionStatus.LowConfidence;
        }

        return new AnnouncementDocument
        {
            Id = $"{announcement.Provider}:{announcement.AnnouncementId}:{hash[..12]}",
            IpoEventId = ipoEvent.Id,
            Reference = announcement,
            LocalPath = path,
            FileHash = hash,
            ExtractedTextHash = text is null ? null : ValueNormalizer.Sha256(text),
            ExtractionStatus = status,
            ParserVersion = AnnouncementFieldParser.Version,
            ParsedFields = fields,
            DownloadedAt = ChinaTime.Now(_timeProvider),
        };
    }

    private async Task EnsureSessionAsync(DownloadSession session, CancellationToken cancellationToken)
    {
        if (_initializedSessions.Contains(session.Key))
        {
            return;
        }

        await _sessionGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            if (_initializedSessions.Contains(session.Key))
            {
                return;
            }

            using var request = new HttpRequestMessage(HttpMethod.Get, session.LandingPage);
            request.Headers.Referrer = session.Referrer;
            using var response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                cancellationToken).ConfigureAwait(false);
            HttpResponseGuard.EnsureSuccess(response, ChinaTime.Now(_timeProvider));
            await response.Content.CopyToAsync(Stream.Null, cancellationToken).ConfigureAwait(false);
            _initializedSessions.Add(session.Key);
        }
        finally
        {
            _sessionGate.Release();
        }
    }

    private static DownloadSession? SessionFor(Uri uri)
    {
        if (uri.Host.EndsWith("sse.com.cn", StringComparison.OrdinalIgnoreCase))
        {
            return new DownloadSession(
                "sse",
                new Uri("https://www.sse.com.cn/ipo/listing/"),
                new Uri("https://www.sse.com.cn/"));
        }

        if (uri.Host.EndsWith("cninfo.com.cn", StringComparison.OrdinalIgnoreCase)
            || uri.Host.EndsWith("szse.cn", StringComparison.OrdinalIgnoreCase))
        {
            return new DownloadSession(
                "cninfo",
                new Uri("https://www.cninfo.com.cn/new/index"),
                new Uri("https://www.cninfo.com.cn/new/index"));
        }

        if (uri.Host.EndsWith("bseinfo.net", StringComparison.OrdinalIgnoreCase)
            || uri.Host.EndsWith("bse.cn", StringComparison.OrdinalIgnoreCase))
        {
            return new DownloadSession(
                "bse",
                new Uri("https://www.bseinfo.net/newshare/listofissues.html"),
                new Uri("https://www.bseinfo.net/newshare/listofissues.html"));
        }

        return null;
    }

    private static bool ClaimsPdf(MediaTypeHeaderValue? contentType, Uri uri) =>
        contentType?.MediaType?.Contains("pdf", StringComparison.OrdinalIgnoreCase) == true
        || uri.AbsolutePath.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase);

    private static bool HasPdfSignature(ReadOnlySpan<byte> bytes) =>
        bytes.Length >= 5 && bytes[..5].SequenceEqual("%PDF-"u8);

    private static bool IsHtml(MediaTypeHeaderValue? contentType, byte[] bytes)
    {
        if (contentType?.MediaType?.Contains("html", StringComparison.OrdinalIgnoreCase) == true)
        {
            return true;
        }

        var prefix = Encoding.UTF8.GetString(bytes, 0, Math.Min(bytes.Length, 4096)).TrimStart('\uFEFF', ' ', '\t', '\r', '\n');
        return prefix.StartsWith("<!doctype html", StringComparison.OrdinalIgnoreCase)
            || prefix.StartsWith("<html", StringComparison.OrdinalIgnoreCase)
            || prefix.StartsWith("<head", StringComparison.OrdinalIgnoreCase)
            || prefix.StartsWith("<body", StringComparison.OrdinalIgnoreCase);
    }

    private static string ExtractPdfText(byte[] bytes)
    {
        using var stream = new MemoryStream(bytes, writable: false);
        using var document = PdfDocument.Open(stream);
        var builder = new StringBuilder();
        foreach (var page in document.GetPages())
        {
            builder.AppendLine(page.Text);
        }

        return builder.ToString();
    }

    private static string ExtractHtmlText(byte[] bytes)
    {
        var html = Encoding.UTF8.GetString(bytes);
        var withoutScripts = ScriptAndStyle().Replace(html, " ");
        var withoutTags = Tags().Replace(withoutScripts, " ");
        return System.Net.WebUtility.HtmlDecode(withoutTags);
    }

    private static string Sanitize(string value)
    {
        var invalid = Path.GetInvalidFileNameChars();
        return new string(value.Select(c => invalid.Contains(c) || c == ':' ? '_' : c).ToArray());
    }

    [GeneratedRegex(@"<(script|style)\b[^>]*>.*?</\1>", RegexOptions.IgnoreCase | RegexOptions.Singleline | RegexOptions.CultureInvariant)]
    private static partial Regex ScriptAndStyle();

    [GeneratedRegex(@"<[^>]+>", RegexOptions.Singleline | RegexOptions.CultureInvariant)]
    private static partial Regex Tags();

    private readonly record struct DownloadSession(string Key, Uri LandingPage, Uri Referrer);

    public void Dispose() => _sessionGate.Dispose();
}
