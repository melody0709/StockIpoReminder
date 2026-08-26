use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/StockIpoReminder.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        compile_windows_resources();
    }

    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("failed to compile Slint UI");
}

fn compile_windows_resources() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let icon = manifest_dir
        .join("assets/StockIpoReminder.ico")
        .canonicalize()
        .expect("application icon is missing");
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR"));
    let application_manifest = output_dir.join("stock-ipo-reminder.manifest");
    let resource_script = output_dir.join("stock-ipo-reminder.rc");
    let compiled_resource = output_dir.join("stock-ipo-reminder.res");
    let icon_path = icon
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    let manifest_version = windows_manifest_version(
        &env::var("CARGO_PKG_VERSION").expect("missing CARGO_PKG_VERSION"),
    );
    fs::write(
        &application_manifest,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="{manifest_version}" processorArchitecture="amd64" name="StockIpoReminder" type="win32" />
  <description>A 股新股申购提醒</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#
        ),
    )
    .expect("failed to write Windows application manifest");
    let manifest_path = application_manifest
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    fs::write(
        &resource_script,
        format!("1 ICON \"{icon_path}\"\r\n1 24 \"{manifest_path}\"\r\n"),
    )
    .expect("failed to write Windows resource script");

    let mut last_error = None;
    for compiler in ["rc.exe", "llvm-rc.exe"] {
        match Command::new(compiler)
            .arg("/nologo")
            .arg(format!("/fo{}", compiled_resource.display()))
            .arg(&resource_script)
            .status()
        {
            Ok(status) if status.success() => {
                println!(
                    "cargo:rustc-link-arg-bin=StockIpoReminder={}",
                    compiled_resource.display()
                );
                return;
            }
            Ok(status) => last_error = Some(format!("{compiler} exited with {status}")),
            Err(error) => last_error = Some(format!("{compiler}: {error}")),
        }
    }
    panic!(
        "failed to compile Windows application resources: {}",
        last_error.unwrap_or_else(|| "no resource compiler was attempted".into())
    );
}

fn windows_manifest_version(package_version: &str) -> String {
    let numeric_version = package_version.split(['-', '+']).next().unwrap_or("0");
    let mut parts = numeric_version
        .split('.')
        .take(4)
        .map(|part| part.parse::<u16>().unwrap_or(0).to_string())
        .collect::<Vec<_>>();
    while parts.len() < 4 {
        parts.push("0".into());
    }
    parts.join(".")
}
