# mboxextract

`mboxextract` is a Rust command-line utility that extracts MIME attachments from an mbox file and organizes them by the email date.

## Build

```powershell
cargo build --release
```

## Usage

```powershell
.\target\release\mboxextract.exe C:\mail\archive.mbox
.\target\release\mboxextract.exe C:\mail\archive.mbox --output C:\extracted --verbose
```

Attachments are written beneath `./<YYYY-MM-DD>/`. Messages without a usable date header or mbox envelope date are placed under `./unknown/`.

By default, name collisions are resolved with numeric suffixes (for example `report-1.pdf`). Pass `--overwrite` to replace existing files or `--skip-existing` to leave them unchanged. These options cannot be used together.

