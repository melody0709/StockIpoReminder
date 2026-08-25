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
    let resource_script = output_dir.join("stock-ipo-reminder.rc");
    let compiled_resource = output_dir.join("stock-ipo-reminder.res");
    let icon_path = icon
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    fs::write(&resource_script, format!("1 ICON \"{icon_path}\"\r\n"))
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
        "failed to compile Windows icon resource: {}",
        last_error.unwrap_or_else(|| "no resource compiler was attempted".into())
    );
}
