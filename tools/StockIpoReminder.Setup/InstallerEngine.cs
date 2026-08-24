using System.Diagnostics;
using System.IO.Compression;
using System.Reflection;
using Microsoft.Data.Sqlite;

namespace StockIpoReminder.Setup;

internal static class InstallerEngine
{
    public static async Task<SetupResult> InstallAsync(SetupOptions options, CancellationToken cancellationToken = default)
    {
        try
        {
            EnsureSupportedPlatform();
            if (WindowsIntegration.IsApplicationRunning(options.InstallDirectory))
            {
                return Failure(20, "install", "应用仍在运行。请先从系统托盘退出 A 股新股申购提醒，再重新安装或升级。", options);
            }

            var existingManifestPath = Path.Combine(options.InstallDirectory, ProductConstants.InstallManifestName);
            InstallationManifest? existingManifest = null;
            if (Directory.Exists(options.InstallDirectory))
            {
                if (File.Exists(existingManifestPath))
                {
                    existingManifest = await SetupJson.ReadAsync<InstallationManifest>(existingManifestPath, cancellationToken).ConfigureAwait(false);
                    ValidateManifest(existingManifest);
                    ValidateManifestLocation(existingManifest, existingManifestPath);
                    ValidateUpgradeIdentity(existingManifest, options);
                }
                else if (Directory.EnumerateFileSystemEntries(options.InstallDirectory).Any())
                {
                    return Failure(21, "install", "目标安装目录已存在且不是可识别的本程序安装目录。为避免覆盖其他文件，安装已停止。", options);
                }
            }

            ValidateDataMarkerForInstall(options);
            string? backupPath = null;
            if (existingManifest is not null)
            {
                backupPath = await BackupDatabaseBeforeUpgradeAsync(existingManifest.DataRoot, cancellationToken).ConfigureAwait(false);
            }

            var installParent = Directory.GetParent(options.InstallDirectory)?.FullName
                ?? throw new InvalidOperationException("安装目录不能是文件系统根目录。");
            Directory.CreateDirectory(installParent);
            var stageDirectory = Path.Combine(installParent, $".{ProductConstants.ProductId}.stage-{Guid.NewGuid():N}");
            var previousDirectory = Path.Combine(installParent, $".{ProductConstants.ProductId}.previous-{Guid.NewGuid():N}");
            var oldMoved = false;
            var newActivated = false;
            InstallationManifest? newManifest = null;
            try
            {
                Directory.CreateDirectory(stageDirectory);
                await ExtractPayloadAsync(stageDirectory, cancellationToken).ConfigureAwait(false);
                ValidatePayload(stageDirectory);

                newManifest = new InstallationManifest
                {
                    ProductId = ProductConstants.ProductId,
                    DisplayName = options.DisplayName,
                    Version = SetupJson.ProductVersion,
                    InstanceId = options.InstanceId,
                    InstallDirectory = options.InstallDirectory,
                    DataRoot = options.DataRoot,
                    RegistryKeyName = options.RegistryKeyName,
                    StartMenuShortcutPath = WindowsIntegration.GetStartMenuShortcutPath(options.StartMenuShortcutName),
                    AutoStartTaskName = options.AutoStartTaskName,
                    InstalledAtUtc = DateTimeOffset.UtcNow,
                };
                await SetupJson.WriteAtomicAsync(
                    Path.Combine(stageDirectory, ProductConstants.InstallManifestName),
                    newManifest,
                    cancellationToken).ConfigureAwait(false);

                if (Directory.Exists(options.InstallDirectory))
                {
                    Directory.Move(options.InstallDirectory, previousDirectory);
                    oldMoved = true;
                }

                Directory.Move(stageDirectory, options.InstallDirectory);
                newActivated = true;
                WindowsIntegration.CreateStartMenuShortcut(newManifest);
                WindowsIntegration.RegisterUninstall(newManifest);
                await WriteDataMarkerAsync(newManifest, cancellationToken).ConfigureAwait(false);

                if (oldMoved && Directory.Exists(previousDirectory))
                {
                    Directory.Delete(previousDirectory, recursive: true);
                }

                return new SetupResult(
                    true,
                    0,
                    "install",
                    existingManifest is null ? "安装完成。" : "升级完成，原数据库已在替换程序文件前备份。",
                    options.InstallDirectory,
                    options.DataRoot,
                    backupPath,
                    DataPreserved: true);
            }
            catch
            {
                if (newManifest is not null)
                {
                    TryRemoveIntegration(newManifest);
                }

                if (newActivated && Directory.Exists(options.InstallDirectory))
                {
                    Directory.Delete(options.InstallDirectory, recursive: true);
                }

                if (oldMoved && Directory.Exists(previousDirectory))
                {
                    Directory.Move(previousDirectory, options.InstallDirectory);
                    if (existingManifest is not null)
                    {
                        TryRestoreIntegration(existingManifest);
                    }
                }

                throw;
            }
            finally
            {
                if (Directory.Exists(stageDirectory))
                {
                    Directory.Delete(stageDirectory, recursive: true);
                }
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            return Failure(1, "install", $"安装失败：{ex.Message}", options);
        }
    }

    public static async Task<SetupResult> UninstallAsync(SetupOptions options, CancellationToken cancellationToken = default)
    {
        try
        {
            var manifestPath = options.ManifestFile
                ?? Path.Combine(options.InstallDirectory, ProductConstants.InstallManifestName);
            if (!File.Exists(manifestPath))
            {
                return Failure(30, "uninstall", "未找到安装清单，无法安全确定需要卸载的目录。", options);
            }

            var manifest = await SetupJson.ReadAsync<InstallationManifest>(manifestPath, cancellationToken).ConfigureAwait(false);
            ValidateManifest(manifest);
            ValidateManifestLocation(manifest, manifestPath);
            if (WindowsIntegration.IsApplicationRunning(manifest.InstallDirectory))
            {
                return new SetupResult(
                    false,
                    31,
                    "uninstall",
                    "应用仍在运行。请先从系统托盘退出 A 股新股申购提醒，再重新卸载。",
                    manifest.InstallDirectory,
                    manifest.DataRoot);
            }

            if (options.DeleteData && !options.ConfirmDeleteData)
            {
                return new SetupResult(
                    false,
                    32,
                    "uninstall",
                    "删除本地数据需要显式二次确认。",
                    manifest.InstallDirectory,
                    manifest.DataRoot);
            }

            await WindowsIntegration.DeleteScheduledTaskAsync(manifest.AutoStartTaskName, cancellationToken).ConfigureAwait(false);
            WindowsIntegration.RemoveStartMenuShortcut(manifest.StartMenuShortcutPath);
            WindowsIntegration.RemoveUninstallRegistration(manifest.RegistryKeyName);
            DeleteInstallationDirectory(manifest.InstallDirectory);

            var dataPreserved = true;
            if (options.DeleteData)
            {
                await DeleteDataDirectoryAsync(manifest, cancellationToken).ConfigureAwait(false);
                dataPreserved = false;
            }

            return new SetupResult(
                true,
                0,
                "uninstall",
                dataPreserved ? "卸载完成，本地数据库、设置、公告缓存和备份已保留。" : "卸载完成，本地数据已按二次确认删除。",
                manifest.InstallDirectory,
                manifest.DataRoot,
                DataPreserved: dataPreserved);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            return Failure(1, "uninstall", $"卸载失败：{ex.Message}", options);
        }
    }

    private static void EnsureSupportedPlatform()
    {
        if (!OperatingSystem.IsWindowsVersionAtLeast(10, 0, 19041))
        {
            throw new PlatformNotSupportedException("需要 Windows 10 2004（Build 19041）或更高版本。");
        }

        if (!Environment.Is64BitOperatingSystem)
        {
            throw new PlatformNotSupportedException("首版仅支持 x64 Windows。");
        }
    }

    private static async Task ExtractPayloadAsync(string stageDirectory, CancellationToken cancellationToken)
    {
        await using var payload = Assembly.GetExecutingAssembly().GetManifestResourceStream(ProductConstants.PayloadResourceName)
            ?? throw new InvalidOperationException("安装包不包含应用负载。请使用正式发布的 Setup 文件，而不是普通工程构建输出。");
        using var archive = new ZipArchive(payload, ZipArchiveMode.Read, leaveOpen: false);
        var stageRoot = Path.GetFullPath(stageDirectory) + Path.DirectorySeparatorChar;
        foreach (var entry in archive.Entries)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var destinationPath = Path.GetFullPath(Path.Combine(stageDirectory, entry.FullName));
            if (!destinationPath.StartsWith(stageRoot, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException($"安装负载包含越界路径：{entry.FullName}");
            }

            if (string.IsNullOrEmpty(entry.Name))
            {
                Directory.CreateDirectory(destinationPath);
                continue;
            }

            Directory.CreateDirectory(Path.GetDirectoryName(destinationPath)!);
            await using var source = entry.Open();
            await using var destination = new FileStream(
                destinationPath,
                FileMode.CreateNew,
                FileAccess.Write,
                FileShare.None,
                bufferSize: 64 * 1024,
                FileOptions.Asynchronous | FileOptions.SequentialScan);
            await source.CopyToAsync(destination, cancellationToken).ConfigureAwait(false);
        }
    }

    private static void ValidatePayload(string stageDirectory)
    {
        var executablePath = Path.Combine(stageDirectory, ProductConstants.ExecutableName);
        var uninstallerPath = Path.Combine(stageDirectory, ProductConstants.UninstallerName);
        if (!File.Exists(executablePath) || !File.Exists(uninstallerPath))
        {
            throw new InvalidDataException("安装负载缺少主程序或卸载程序。");
        }

        var versionInfo = FileVersionInfo.GetVersionInfo(executablePath);
        if (string.IsNullOrWhiteSpace(versionInfo.FileVersion)
            || !versionInfo.FileVersion.StartsWith(SetupJson.ProductVersion, StringComparison.Ordinal))
        {
            throw new InvalidDataException($"应用负载版本与安装器不一致：安装器 {SetupJson.ProductVersion}，应用 {versionInfo.FileVersion ?? "unknown"}。");
        }
    }

    private static void ValidateManifest(InstallationManifest manifest)
    {
        if (!string.Equals(manifest.ProductId, ProductConstants.ProductId, StringComparison.Ordinal)
            || string.IsNullOrWhiteSpace(manifest.InstanceId)
            || string.IsNullOrWhiteSpace(manifest.InstallDirectory)
            || string.IsNullOrWhiteSpace(manifest.DataRoot))
        {
            throw new InvalidDataException("安装清单身份或路径无效。");
        }

        EnsureSafeDirectory(manifest.InstallDirectory, "安装目录");
        EnsureSafeDirectory(manifest.DataRoot, "数据目录");

        var isDefaultInstance = string.Equals(manifest.InstanceId, SetupOptions.DefaultInstanceId, StringComparison.OrdinalIgnoreCase);
        var expectedDisplayName = isDefaultInstance
            ? ProductConstants.DisplayName
            : $"{ProductConstants.DisplayName} ({manifest.InstanceId})";
        var expectedRegistryKey = isDefaultInstance
            ? ProductConstants.ProductId
            : $"{ProductConstants.ProductId}-{manifest.InstanceId}";
        var expectedShortcutName = isDefaultInstance
            ? $"{ProductConstants.DisplayName}.lnk"
            : $"{ProductConstants.DisplayName} ({manifest.InstanceId}).lnk";
        var expectedShortcutPath = WindowsIntegration.GetStartMenuShortcutPath(expectedShortcutName);
        var expectedTaskName = SetupOptions.CreateAutoStartTaskName(manifest.DataRoot);
        if (!string.Equals(manifest.DisplayName, expectedDisplayName, StringComparison.Ordinal)
            || !string.Equals(manifest.RegistryKeyName, expectedRegistryKey, StringComparison.Ordinal)
            || !SetupOptions.PathsEqual(manifest.StartMenuShortcutPath, expectedShortcutPath)
            || !string.Equals(manifest.AutoStartTaskName, expectedTaskName, StringComparison.Ordinal))
        {
            throw new InvalidDataException("安装清单派生身份与实例标识或数据目录不一致。");
        }
    }

    private static void ValidateManifestLocation(InstallationManifest manifest, string manifestPath)
    {
        var manifestDirectory = Path.GetDirectoryName(Path.GetFullPath(manifestPath))
            ?? throw new InvalidDataException("安装清单路径没有父目录。");
        if (!SetupOptions.PathsEqual(manifestDirectory, manifest.InstallDirectory))
        {
            throw new InvalidDataException("安装清单所在目录与清单记录的安装目录不一致。");
        }
    }

    private static void ValidateUpgradeIdentity(InstallationManifest manifest, SetupOptions options)
    {
        if (!SetupOptions.PathsEqual(manifest.InstallDirectory, options.InstallDirectory))
        {
            throw new InvalidOperationException("现有安装清单与目标安装目录不一致，拒绝升级。");
        }

        if (!string.Equals(manifest.InstanceId, options.InstanceId, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException("同一安装目录不能切换到不同实例标识。请使用新的安装目录。");
        }

        if (!SetupOptions.PathsEqual(manifest.DataRoot, options.DataRoot))
        {
            throw new InvalidOperationException("同一安装目录不能切换到不同数据目录。请使用新的安装目录或保持原数据目录。");
        }
    }

    private static void ValidateDataMarkerForInstall(SetupOptions options)
    {
        var markerPath = Path.Combine(options.DataRoot, ProductConstants.DataMarkerName);
        if (!File.Exists(markerPath))
        {
            return;
        }

        var marker = SetupJson.ReadAsync<DataDirectoryMarker>(markerPath).GetAwaiter().GetResult();
        if (!string.Equals(marker.ProductId, ProductConstants.ProductId, StringComparison.Ordinal)
            || !string.Equals(marker.InstanceId, options.InstanceId, StringComparison.OrdinalIgnoreCase)
            || !SetupOptions.PathsEqual(marker.DataRoot, options.DataRoot))
        {
            throw new InvalidOperationException("目标数据目录已由另一个安装实例占用。请更换数据目录或实例标识。");
        }
    }

    private static async Task WriteDataMarkerAsync(InstallationManifest manifest, CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(manifest.DataRoot);
        var marker = new DataDirectoryMarker
        {
            ProductId = ProductConstants.ProductId,
            InstanceId = manifest.InstanceId,
            DataRoot = manifest.DataRoot,
            CreatedAtUtc = DateTimeOffset.UtcNow,
        };
        await SetupJson.WriteAtomicAsync(
            Path.Combine(manifest.DataRoot, ProductConstants.DataMarkerName),
            marker,
            cancellationToken).ConfigureAwait(false);
    }

    private static async Task<string?> BackupDatabaseBeforeUpgradeAsync(string dataRoot, CancellationToken cancellationToken)
    {
        var databasePath = Path.Combine(dataRoot, "stock-ipo-reminder.db");
        if (!File.Exists(databasePath))
        {
            return null;
        }

        var backupDirectory = Path.Combine(dataRoot, "backups");
        Directory.CreateDirectory(backupDirectory);
        var timestamp = DateTimeOffset.Now.ToString("yyyyMMdd-HHmmss", System.Globalization.CultureInfo.InvariantCulture);
        var backupPath = Path.Combine(backupDirectory, $"pre-upgrade-{SetupJson.ProductVersion}-{timestamp}.db");
        var temporaryPath = backupPath + $".{Guid.NewGuid():N}.tmp";
        var sourceBuilder = new SqliteConnectionStringBuilder
        {
            DataSource = databasePath,
            Mode = SqliteOpenMode.ReadOnly,
            Pooling = false,
        };
        var destinationBuilder = new SqliteConnectionStringBuilder
        {
            DataSource = temporaryPath,
            Mode = SqliteOpenMode.ReadWriteCreate,
            Pooling = false,
        };
        await using (var source = new SqliteConnection(sourceBuilder.ToString()))
        await using (var destination = new SqliteConnection(destinationBuilder.ToString()))
        {
            await source.OpenAsync(cancellationToken).ConfigureAwait(false);
            await destination.OpenAsync(cancellationToken).ConfigureAwait(false);
            source.BackupDatabase(destination);
            await using var integrityCommand = destination.CreateCommand();
            integrityCommand.CommandText = "PRAGMA integrity_check;";
            var integrity = Convert.ToString(await integrityCommand.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false), System.Globalization.CultureInfo.InvariantCulture);
            if (!string.Equals(integrity, "ok", StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException($"升级前数据库备份完整性检查失败：{integrity ?? "unknown"}");
            }
        }

        File.Move(temporaryPath, backupPath);
        return backupPath;
    }

    private static void DeleteInstallationDirectory(string installDirectory)
    {
        EnsureSafeDirectory(installDirectory, "安装目录");
        var manifestPath = Path.Combine(installDirectory, ProductConstants.InstallManifestName);
        if (Directory.Exists(installDirectory) && !File.Exists(manifestPath))
        {
            throw new InvalidOperationException("安装目录缺少产品清单，拒绝递归删除。");
        }

        if (Directory.Exists(installDirectory))
        {
            Directory.Delete(installDirectory, recursive: true);
        }
    }

    private static async Task DeleteDataDirectoryAsync(InstallationManifest manifest, CancellationToken cancellationToken)
    {
        EnsureSafeDirectory(manifest.DataRoot, "数据目录");
        var markerPath = Path.Combine(manifest.DataRoot, ProductConstants.DataMarkerName);
        if (!File.Exists(markerPath))
        {
            throw new InvalidOperationException("数据目录缺少本程序所有权标记，拒绝递归删除。");
        }

        var marker = await SetupJson.ReadAsync<DataDirectoryMarker>(markerPath, cancellationToken).ConfigureAwait(false);
        if (!string.Equals(marker.ProductId, ProductConstants.ProductId, StringComparison.Ordinal)
            || !string.Equals(marker.InstanceId, manifest.InstanceId, StringComparison.OrdinalIgnoreCase)
            || !SetupOptions.PathsEqual(marker.DataRoot, manifest.DataRoot))
        {
            throw new InvalidOperationException("数据目录所有权标记不匹配，拒绝递归删除。");
        }

        Directory.Delete(manifest.DataRoot, recursive: true);
    }

    private static void EnsureSafeDirectory(string path, string description)
    {
        var fullPath = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
        var root = Path.TrimEndingDirectorySeparator(Path.GetPathRoot(fullPath) ?? string.Empty);
        if (string.IsNullOrWhiteSpace(root)
            || string.Equals(fullPath, root, StringComparison.OrdinalIgnoreCase)
            || string.Equals(fullPath, Path.TrimEndingDirectorySeparator(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile)), StringComparison.OrdinalIgnoreCase)
            || string.Equals(fullPath, Path.TrimEndingDirectorySeparator(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData)), StringComparison.OrdinalIgnoreCase)
            || string.Equals(fullPath, Path.TrimEndingDirectorySeparator(Path.GetTempPath()), StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException($"{description}过于宽泛，拒绝执行递归文件操作：{fullPath}");
        }
    }

    private static void TryRemoveIntegration(InstallationManifest manifest)
    {
        try
        {
            WindowsIntegration.RemoveStartMenuShortcut(manifest.StartMenuShortcutPath);
            WindowsIntegration.RemoveUninstallRegistration(manifest.RegistryKeyName);
        }
        catch
        {
        }
    }

    private static void TryRestoreIntegration(InstallationManifest manifest)
    {
        try
        {
            WindowsIntegration.CreateStartMenuShortcut(manifest);
            WindowsIntegration.RegisterUninstall(manifest);
        }
        catch
        {
        }
    }

    private static SetupResult Failure(int exitCode, string operation, string message, SetupOptions options) => new(
        false,
        exitCode,
        operation,
        message,
        options.InstallDirectory,
        options.DataRoot);
}
