using StockIpoReminder.Core.Services;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class ValueNormalizerTests
{
    [DataTestMethod]
    [DataRow(null)]
    [DataRow("")]
    [DataRow("-")]
    [DataRow("--")]
    [DataRow("N/A")]
    public void Missing_Text_Is_Null(string? value) => Assert.IsNull(ValueNormalizer.Text(value));

    [TestMethod]
    public void TenThousand_Shares_Are_Converted_To_Shares() =>
        Assert.AreEqual(5500, ValueNormalizer.Integer("0.55", 10_000m));

    [TestMethod]
    public void Parses_Chinese_Date() =>
        Assert.AreEqual(new DateOnly(2026, 8, 26), ValueNormalizer.Date("2026年8月26日"));
}
