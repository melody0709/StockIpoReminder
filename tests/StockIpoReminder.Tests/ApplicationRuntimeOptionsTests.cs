using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Tests;

[TestClass]
public sealed class ApplicationRuntimeOptionsTests
{
    [TestMethod]
    public void Defaults_Use_Local_Data_Root_And_Stable_Production_Task_Name()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");

        var options = ApplicationRuntimeOptions.Parse([], defaultRoot, environmentDataRoot: null);

        Assert.AreEqual(Path.GetFullPath(defaultRoot), options.DataRoot);
        Assert.IsTrue(options.UsesDefaultDataRoot);
        Assert.IsFalse(options.Background);
        Assert.AreEqual("StockIpoReminder", options.AutoStartTaskName);
        Assert.AreEqual("--background", options.AutoStartArguments);
        StringAssert.StartsWith(options.MutexName, "Local\\StockIpoReminder.");
    }

    [TestMethod]
    public void Explicit_Data_Root_Overrides_Environment_And_Is_Propagated_To_AutoStart()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var environmentRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-environment");
        var explicitRoot = Path.Combine(Path.GetTempPath(), "stock ipo explicit");

        var options = ApplicationRuntimeOptions.Parse(
            ["--background", "--data-root", explicitRoot],
            defaultRoot,
            environmentRoot);

        Assert.AreEqual(Path.GetFullPath(explicitRoot), options.DataRoot);
        Assert.IsFalse(options.UsesDefaultDataRoot);
        Assert.IsTrue(options.Background);
        StringAssert.StartsWith(options.AutoStartTaskName, "StockIpoReminder-");
        StringAssert.Contains(options.AutoStartArguments, "--background --data-root");
        StringAssert.Contains(options.AutoStartArguments, $"\"{Path.GetFullPath(explicitRoot)}\"");
    }

    [TestMethod]
    public void Environment_Data_Root_Is_Used_When_Command_Line_Does_Not_Override_It()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var environmentRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-environment");

        var options = ApplicationRuntimeOptions.Parse([], defaultRoot, environmentRoot);

        Assert.AreEqual(Path.GetFullPath(environmentRoot), options.DataRoot);
        Assert.IsFalse(options.UsesDefaultDataRoot);
    }

    [TestMethod]
    public void Probe_And_Automatic_Exit_Options_Are_Parsed_For_Isolated_Smoke()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var readyFile = Path.Combine(Path.GetTempPath(), "stock-ipo-smoke", "ready.json");

        var options = ApplicationRuntimeOptions.Parse(
            ["--smoke-mode", "--smoke-enable-autostart", "--ready-file", readyFile, "--exit-after-seconds=12.5"],
            defaultRoot,
            environmentDataRoot: null);

        Assert.AreEqual(Path.GetFullPath(readyFile), options.ReadyFile);
        Assert.AreEqual(TimeSpan.FromSeconds(12.5), options.ExitAfter);
        Assert.IsTrue(options.SmokeMode);
        Assert.IsTrue(options.SmokeEnableAutoStart);
    }

    [TestMethod]
    public void Ui_Smoke_Options_Require_Isolated_Smoke_And_Are_Parsed()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var reportFile = Path.Combine(Path.GetTempPath(), "stock-ipo-ui-smoke", "report.json");

        var options = ApplicationRuntimeOptions.Parse(
            ["--smoke-mode", "--smoke-seed-scenarios", "--ui-smoke-report", reportFile],
            defaultRoot,
            environmentDataRoot: null);

        Assert.IsTrue(options.SmokeMode);
        Assert.IsTrue(options.SmokeSeedScenarios);
        Assert.AreEqual(Path.GetFullPath(reportFile), options.UiSmokeReport);
    }

    [TestMethod]
    public void Process_Smoke_Options_Are_Parsed_For_Prepare_And_Verify_Stages()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var prepareReport = Path.Combine(Path.GetTempPath(), "stock-ipo-process-smoke", "prepare.json");
        var verifyReport = Path.Combine(Path.GetTempPath(), "stock-ipo-process-smoke", "verify.json");

        var prepare = ApplicationRuntimeOptions.Parse(
            ["--smoke-mode", "--smoke-seed-scenarios", "--process-smoke-stage", "prepare", "--process-smoke-report", prepareReport],
            defaultRoot,
            environmentDataRoot: null);
        var verify = ApplicationRuntimeOptions.Parse(
            ["--smoke-mode", "--process-smoke-stage=verify", "--process-smoke-report", verifyReport],
            defaultRoot,
            environmentDataRoot: null);

        Assert.AreEqual(ProcessSmokeStage.Prepare, prepare.ProcessSmokePhase);
        Assert.AreEqual(Path.GetFullPath(prepareReport), prepare.ProcessSmokeReport);
        Assert.AreEqual(ProcessSmokeStage.Verify, verify.ProcessSmokePhase);
        Assert.AreEqual(Path.GetFullPath(verifyReport), verify.ProcessSmokeReport);
    }

    [TestMethod]
    public void Recovery_Smoke_Option_Requires_Smoke_Mode_And_Is_Parsed()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var reportFile = Path.Combine(Path.GetTempPath(), "stock-ipo-recovery-smoke", "report.json");

        var options = ApplicationRuntimeOptions.Parse(
            ["--smoke-mode", "--recovery-smoke-report", reportFile],
            defaultRoot,
            environmentDataRoot: null);

        Assert.IsTrue(options.SmokeMode);
        Assert.AreEqual(Path.GetFullPath(reportFile), options.RecoverySmokeReport);
    }

    [TestMethod]
    public void Same_Data_Root_Has_The_Same_Instance_Key_Ignoring_Path_Case()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");
        var first = ApplicationRuntimeOptions.Parse(["--data-root", defaultRoot.ToUpperInvariant()], defaultRoot, null);
        var second = ApplicationRuntimeOptions.Parse(["--data-root", defaultRoot.ToLowerInvariant()], defaultRoot, null);

        Assert.AreEqual(first.InstanceKey, second.InstanceKey);
        Assert.AreEqual(first.MutexName, second.MutexName);
    }

    [TestMethod]
    public void Missing_Or_Invalid_Option_Value_Is_Rejected()
    {
        var defaultRoot = Path.Combine(Path.GetTempPath(), "stock-ipo-default");

        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--data-root"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--exit-after-seconds", "0"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-enable-autostart"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-seed-scenarios"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-mode", "--ui-smoke-report", "ui.json"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-mode", "--process-smoke-stage", "prepare"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-mode", "--process-smoke-report", "process.json"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-mode", "--process-smoke-stage", "unknown", "--process-smoke-report", "process.json"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-mode", "--process-smoke-stage", "prepare", "--process-smoke-report", "process.json"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--recovery-smoke-report", "recovery.json"], defaultRoot, null));
        Assert.ThrowsExactly<ArgumentException>(() =>
            ApplicationRuntimeOptions.Parse(["--smoke-mode", "--smoke-seed-scenarios", "--ui-smoke-report", "ui.json", "--recovery-smoke-report", "recovery.json"], defaultRoot, null));
    }
}
