using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class OutboundNetworkPolicyTests
{
    [TestMethod]
    [DataRow("https://datacenter-web.eastmoney.com/api/data/v1/get")]
    [DataRow("https://query.sse.com.cn/commonQuery.do")]
    [DataRow("https://www.sse.com.cn/disclosure/example.pdf")]
    [DataRow("https://static.cninfo.com.cn/finalpage/example.PDF")]
    [DataRow("https://disc.static.szse.cn/download/example.PDF")]
    [DataRow("https://www.bseinfo.net/disclosure/example.pdf")]
    [DataRow("https://www.microsoft.com/")]
    [DataRow("https://www.cloudflare.com/")]
    public void KnownPublicDataHostsAreAllowed(string value)
    {
        OutboundNetworkPolicy.EnsureAllowedHttps(new Uri(value));
    }

    [TestMethod]
    [DataRow("http://www.sse.com.cn/example.pdf")]
    [DataRow("https://evil-sse.com.cn/example.pdf")]
    [DataRow("https://www.sse.com.cn.example.invalid/example.pdf")]
    [DataRow("https://example.invalid/example.pdf")]
    public void NonHttpsOrUnknownHostsAreRejected(string value)
    {
        Assert.ThrowsExactly<InvalidDataException>(() =>
            OutboundNetworkPolicy.EnsureAllowedHttps(new Uri(value)));
    }

    [TestMethod]
    [DataRow("https://www.sse.com.cn/disclosure/example.pdf")]
    [DataRow("https://static.cninfo.com.cn/finalpage/example.PDF")]
    [DataRow("https://disc.static.szse.cn/download/example.PDF")]
    [DataRow("https://www.bseinfo.net/disclosure/example.pdf")]
    public void OfficialAnnouncementHostsAreAllowed(string value)
    {
        OutboundNetworkPolicy.EnsureAllowedAnnouncementHttps(new Uri(value));
    }

    [TestMethod]
    [DataRow("https://datacenter-web.eastmoney.com/api/data/v1/get")]
    [DataRow("https://query.sse.com.cn/commonQuery.do")]
    [DataRow("https://www.microsoft.com/")]
    [DataRow("https://example.invalid/example.pdf")]
    public void NonAnnouncementHostsCannotBeDownloadedAsEvidence(string value)
    {
        Assert.ThrowsExactly<InvalidDataException>(() =>
            OutboundNetworkPolicy.EnsureAllowedAnnouncementHttps(new Uri(value)));
    }
}
