using System.Net;
using System.Net.Http.Headers;
using System.Text;
using StockIpoReminder.Core.Domain;
using StockIpoReminder.Infrastructure.Announcements;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class AnnouncementProcessorTests
{
    [TestMethod]
    public async Task Html_Announcement_Is_Saved_Hashed_And_Parsed_With_Evidence()
    {
        using var storage = new TemporaryDirectory();
        using var client = new HttpClient(new SequenceHandler(SseLandingResponse(), HtmlResponse("""
            <html><style>.hidden { display:none }</style><body>
            证券代码：601200；申购代码：731200；网上申购日为2026年8月26日；
            发行价格为人民币12.34元；申购上限1.5万股；每一个申购单位为500股；
            网上申购时间为9:30-11:30、13:00-15:00。
            </body></html>
            """)));
        var processor = CreateProcessor(client, storage.Path);

        var document = await processor.DownloadAndParseAsync(Event(), Reference("html-1", "https://www.sse.com.cn/test/notice"), CancellationToken.None);

        Assert.AreEqual(ExtractionStatus.Extracted, document.ExtractionStatus);
        Assert.IsTrue(File.Exists(document.LocalPath));
        Assert.IsTrue(document.LocalPath.EndsWith(".html", StringComparison.OrdinalIgnoreCase));
        Assert.HasCount(64, document.FileHash);
        var applyCode = AssertExactlyOne(document.ParsedFields.Where(static field => field.Name == "ApplyCode").ToArray());
        Assert.AreEqual("731200", applyCode.Value);
        Assert.IsNotNull(applyCode.Evidence);
        Assert.IsNotNull(applyCode.CharacterOffset);
        Assert.AreEqual("15000", AssertExactlyOne(document.ParsedFields.Where(static field => field.Name == "MaxApplyQuantity").ToArray()).Value);
    }

    [TestMethod]
    public async Task Pdf_Content_Is_Detected_From_Magic_Bytes_And_Text_Is_Extracted()
    {
        using var storage = new TemporaryDirectory();
        using var client = new HttpClient(new SequenceHandler(
            SseLandingResponse(),
            BinaryResponse(BuildMinimalPdf("IPO official notice"), "application/octet-stream")));
        var processor = CreateProcessor(client, storage.Path);

        var document = await processor.DownloadAndParseAsync(
            Event(),
            Reference("pdf-1", "https://www.sse.com.cn/test/download?id=pdf-1"),
            CancellationToken.None);

        Assert.IsTrue(document.LocalPath.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase));
        Assert.AreEqual(ExtractionStatus.LowConfidence, document.ExtractionStatus);
        Assert.IsNotNull(document.ExtractedTextHash);
        Assert.AreEqual(0, document.ParsedFields.Count);
    }

    [TestMethod]
    public async Task Pdf_Url_Returning_Html_Is_Rejected_And_Not_Persisted()
    {
        using var storage = new TemporaryDirectory();
        using var client = new HttpClient(new SequenceHandler(
            HtmlResponse("<html><body>SSE landing page</body></html>"),
            HtmlResponse("<html><body>WAF request id raw-secret-marker</body></html>")));
        var processor = CreateProcessor(client, storage.Path);

        var error = await Assert.ThrowsExactlyAsync<InvalidDataException>(() => processor.DownloadAndParseAsync(
            Event(),
            Reference(
                "sse-pseudo-pdf",
                "https://www.sse.com.cn/disclosure/listedinfo/announcement/c/new/2026-08-18/603448_20260818_JTRF.pdf"),
            CancellationToken.None));

        StringAssert.Contains(error.Message, "缺少 %PDF 文件签名");
        StringAssert.Contains(error.Message, "host=www.sse.com.cn");
        Assert.IsFalse(error.Message.Contains("raw-secret-marker", StringComparison.Ordinal));
        Assert.IsFalse(Directory.EnumerateFileSystemEntries(storage.Path).Any());
    }

    [TestMethod]
    public async Task Pdf_Content_Type_Without_Magic_Bytes_Is_Rejected()
    {
        using var storage = new TemporaryDirectory();
        using var client = new HttpClient(new SequenceHandler(
            SseLandingResponse(),
            BinaryResponse(
                Encoding.UTF8.GetBytes("<html><body>temporary error</body></html>"),
                "application/pdf")));
        var processor = CreateProcessor(client, storage.Path);

        var error = await Assert.ThrowsExactlyAsync<InvalidDataException>(() => processor.DownloadAndParseAsync(
            Event(),
            Reference("bad-pdf-content-type", "https://www.sse.com.cn/test/download?id=bad-pdf"),
            CancellationToken.None));

        StringAssert.Contains(error.Message, "contentType=application/pdf");
        Assert.IsFalse(Directory.EnumerateFileSystemEntries(storage.Path).Any());
    }

    [TestMethod]
    public async Task Same_Url_With_Changed_Content_Creates_A_New_Hashed_Document_Version()
    {
        using var storage = new TemporaryDirectory();
        using var client = new HttpClient(new SequenceHandler(
            SseLandingResponse(),
            HtmlResponse("<html><body>发行价格为10.10元</body></html>"),
            HtmlResponse("<html><body>发行价格为11.20元</body></html>")));
        var processor = CreateProcessor(client, storage.Path);
        var reference = Reference("same-id", "https://www.sse.com.cn/test/same-url");

        var first = await processor.DownloadAndParseAsync(Event(), reference, CancellationToken.None);
        var second = await processor.DownloadAndParseAsync(Event(), reference, CancellationToken.None);

        Assert.AreNotEqual(first.FileHash, second.FileHash);
        Assert.AreNotEqual(first.Id, second.Id);
        Assert.AreNotEqual(first.LocalPath, second.LocalPath);
        Assert.IsTrue(File.Exists(first.LocalPath));
        Assert.IsTrue(File.Exists(second.LocalPath));
        Assert.AreEqual("10.10", AssertExactlyOne(first.ParsedFields).Value);
        Assert.AreEqual("11.20", AssertExactlyOne(second.ParsedFields).Value);
    }

    [TestMethod]
    public async Task Html_Without_Critical_Fields_Is_Explicitly_Low_Confidence()
    {
        using var storage = new TemporaryDirectory();
        using var client = new HttpClient(new SequenceHandler(
            SseLandingResponse(),
            HtmlResponse("<html><body>欢迎访问发行人网站。</body></html>")));
        var processor = CreateProcessor(client, storage.Path);

        var document = await processor.DownloadAndParseAsync(Event(), Reference("low-1", "https://www.sse.com.cn/test/low"), CancellationToken.None);

        Assert.AreEqual(ExtractionStatus.LowConfidence, document.ExtractionStatus);
        Assert.AreEqual(0, document.ParsedFields.Count);
    }

    private static AnnouncementProcessor CreateProcessor(HttpClient client, string storagePath) =>
        new(client, new AnnouncementOptions { StorageDirectory = storagePath }, new AnnouncementFieldParser(), TimeProvider.System);

    private static IpoEvent Event() => new()
    {
        Id = "shanghai:601200",
        Exchange = Exchange.Shanghai,
        Board = Board.Main,
        SecurityCode = "601200",
        ApplyCode = "731200",
        Name = "测试股份",
        ApplyDate = new DateOnly(2026, 8, 26),
        Status = IssueStatus.Upcoming,
        LifecycleStatus = IpoLifecycleStatus.Scheduled,
        FirstSeenAt = DateTimeOffset.UtcNow,
        UpdatedAt = DateTimeOffset.UtcNow,
    };

    private static AnnouncementReference Reference(string id, string url) => new()
    {
        Provider = "fixture-provider",
        AnnouncementId = id,
        Title = "首次公开发行股票发行公告",
        Url = new Uri(url),
        PublishedAt = new DateTimeOffset(2026, 8, 24, 1, 0, 0, TimeSpan.Zero),
        AnnouncementType = "发行公告",
    };

    private static HttpResponseMessage HtmlResponse(string html) => new(HttpStatusCode.OK)
    {
        Content = new StringContent(html, Encoding.UTF8, "text/html"),
    };

    private static HttpResponseMessage SseLandingResponse() =>
        HtmlResponse("<html><body>SSE landing page</body></html>");

    private static HttpResponseMessage BinaryResponse(byte[] bytes, string mediaType)
    {
        var content = new ByteArrayContent(bytes);
        content.Headers.ContentType = new MediaTypeHeaderValue(mediaType);
        return new HttpResponseMessage(HttpStatusCode.OK) { Content = content };
    }

    private static byte[] BuildMinimalPdf(string text)
    {
        var content = $"BT /F1 12 Tf 72 720 Td ({text}) Tj ET";
        using var stream = new MemoryStream();
        using var writer = new StreamWriter(stream, Encoding.ASCII, leaveOpen: true) { NewLine = "\n" };
        writer.WriteLine("%PDF-1.4");
        writer.Flush();
        var offsets = new List<long> { 0 };

        WriteObject(1, "<< /Type /Catalog /Pages 2 0 R >>");
        WriteObject(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        WriteObject(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>");
        WriteObject(4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        offsets.Add(stream.Position);
        writer.WriteLine("5 0 obj");
        writer.WriteLine($"<< /Length {Encoding.ASCII.GetByteCount(content)} >>");
        writer.WriteLine("stream");
        writer.WriteLine(content);
        writer.WriteLine("endstream");
        writer.WriteLine("endobj");
        writer.Flush();

        var xref = stream.Position;
        writer.WriteLine("xref");
        writer.WriteLine("0 6");
        writer.WriteLine("0000000000 65535 f ");
        foreach (var offset in offsets.Skip(1))
        {
            writer.WriteLine($"{offset:0000000000} 00000 n ");
        }

        writer.WriteLine("trailer");
        writer.WriteLine("<< /Size 6 /Root 1 0 R >>");
        writer.WriteLine("startxref");
        writer.WriteLine(xref);
        writer.WriteLine("%%EOF");
        writer.Flush();
        return stream.ToArray();

        void WriteObject(int number, string body)
        {
            offsets.Add(stream.Position);
            writer.WriteLine($"{number} 0 obj");
            writer.WriteLine(body);
            writer.WriteLine("endobj");
            writer.Flush();
        }
    }

    private static T AssertExactlyOne<T>(IReadOnlyList<T> values)
    {
        Assert.AreEqual(1, values.Count);
        return values[0];
    }

    private sealed class SequenceHandler(params HttpResponseMessage[] responses) : HttpMessageHandler
    {
        private int _index;

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (_index >= responses.Length)
            {
                throw new InvalidOperationException("No fixture response remains.");
            }

            return Task.FromResult(responses[_index++]);
        }
    }

    private sealed class TemporaryDirectory : IDisposable
    {
        public TemporaryDirectory() => Path = Directory.CreateTempSubdirectory("stock-ipo-announcement-tests-").FullName;
        public string Path { get; }

        public void Dispose()
        {
            if (Directory.Exists(Path))
            {
                Directory.Delete(Path, recursive: true);
            }
        }
    }
}
