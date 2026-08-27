# Stock IPO Reminder agent instructions

@C:\Users\kawae\.codex\RTK.md

## Project scope

- This repository contains the Rust and Slint implementation of Stock IPO Reminder.
- Preserve user changes already present in the worktree and avoid unrelated edits.
- Treat `Cargo.toml` as the source of truth for the application version.



## Source changes

- Format Rust changes with `rtk cargo fmt`.
- Run `rtk cargo test` for changes that affect Rust, Slint, settings, storage, synchronization, or packaging behavior.
- Keep user-facing documentation and `RELEASE_NOTES.md` consistent with behavior changes.
- Do not manually duplicate the version in the UI; read it from `CARGO_PKG_VERSION`.

## Required release outputs

After implementing and verifying a user-requested application change, regenerate the current release outputs unless the user explicitly asks for source-only changes:

```text
rtk cmd /c build.bat --package
```

Use `--rebuild --package` when a clean release rebuild is requested or when stale generated output is suspected.

Verify these outputs exist and were regenerated from the current source:

```text
build\run\x64-release\StockIpoReminder.exe
build\packages\<version>\StockIpoReminder-<version>-win-x64-portable.zip
build\packages\<version>\StockIpoReminder-<version>-win-x64.msi
build\packages\<version>\release-manifest.json
build\packages\<version>\SHA256SUMS.txt
```

Run the layout validation after packaging:

```text
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1
```

## Completion report

- Report test, release-build, installer, portable-package, and layout-validation results.
- Provide clickable paths to the runnable EXE, MSI, portable ZIP, and checksum file.
- Mention any build warnings or skipped verification explicitly.
