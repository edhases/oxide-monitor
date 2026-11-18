# Building and Packaging Oxide Monitor

This guide covers a local Windows build, the portable release package, the
configuration search order, and how to embed the Windows manifest/icon in a
future build script. It does not change `Cargo.toml` or the application source.

## 1. Prerequisites

### Operating system

- Windows 10 22H2 or newer, or Windows 11, x64.
- PowerShell 5.1 or newer (the packaging script uses `Compress-Archive` and
  `Get-FileHash`).

### Rust

Install the stable MSVC toolchain:

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
rustc -vV
```

`rustc -vV` should report `host: x86_64-pc-windows-msvc`. The workspace uses
edition 2024, so use a current stable Rust; older toolchains will reject the
edition.

### Visual Studio Build Tools

Install **Visual Studio Build Tools 2022** (or the Build Tools component of
Visual Studio) with:

- **Desktop development with C++**
- **MSVC v143 x64/x86 build tools** (latest)
- **Windows 11 SDK** — 10.0.22621 or newer; 10.0.26100 is recommended.
  `rc.exe` and `mt.exe` come from this SDK.

A Developer PowerShell is convenient but not required: Cargo locates MSVC
through `vswhere`. Verify the linker from a normal terminal:

```powershell
where.exe link.exe
```

If `link.exe` is missing, repair the Build Tools installation and make sure the
MSVC x64 component is checked.

### Optional tools

- `sigcheck` (Sysinternals) for checking the embedded manifest and signature.
- `mt.exe` from the Windows SDK for extracting a manifest from the executable.
- `cargo-audit` / `cargo-deny` for dependency checks.

## 2. Build and run

Debug build and run:

```powershell
cargo build -p oxide-app
cargo run -p oxide-app
```

Release build:

```powershell
cargo build --release -p oxide-app
```

The binary target is `oxide-monitor`, so the outputs are:

- `target\debug\oxide-monitor.exe`
- `target\release\oxide-monitor.exe`

The release profile in the root `Cargo.toml` already enables size optimization,
thin LTO, one codegen unit, symbol stripping, and unwind panics.

Run the workspace tests with:

```powershell
cargo test --workspace
```

## 3. Package a portable ZIP

Run the packaging script from the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package.ps1
```

The script:

1. Reads the version from `[workspace.package].version` in the root
   `Cargo.toml` (or uses `-Version`).
2. Runs `cargo build --release -p oxide-app`.
3. Creates `dist\oxide-monitor-<version>\` containing:
   - `oxide-monitor.exe`
   - `config.toml` (copied from `resources\config.toml`; a default sample is
     written there if the file is missing)
   - `README.md`
4. Creates `dist\oxide-monitor-<version>-win-x64.zip`.
5. Prints the SHA-256 of the ZIP and of the executable.

The script stops with an error if the release executable is missing, so a
failed build cannot produce an empty package.

Options:

```powershell
# Override the version used in the directory and archive names
powershell -ExecutionPolicy Bypass -File scripts\package.ps1 -Version 0.1.0

# Package an already-built target\release\oxide-monitor.exe
powershell -ExecutionPolicy Bypass -File scripts\package.ps1 -SkipBuild
```

Verify the printed hash independently:

```powershell
Get-FileHash dist\oxide-monitor-0.1.0-win-x64.zip -Algorithm SHA256
```

`dist\` is ignored by Git. The package is portable in the sense that it has no
installer and no admin requirement; the app itself is still `asInvoker`.

Code signing is not part of `package.ps1` yet. Unsigned builds may trigger
SmartScreen on other machines; sign `oxide-monitor.exe` and the ZIP with the
release certificate before publishing.

## 4. Configuration and portable mode

Default (roaming) location:

```text
%APPDATA%\OxideMonitor\config.toml
```

Current resolution order in `oxide-core` (`Config::resolve_path`):

1. If `OXIDE_PORTABLE` is set (any value), use `config.toml` next to the
   executable.
2. Otherwise, if `config.toml` exists next to the executable, use it.
3. Otherwise, use `%APPDATA%\OxideMonitor\config.toml`.

A command-line `--portable` flag is mentioned in the architecture plan but is
not parsed by the current scaffold; use the environment variable or the
side-by-side file for now.

Because the packaged ZIP places `config.toml` next to the executable, the
package runs in portable mode by default. Logs and history are planned for
`%LOCALAPPDATA%\OxideMonitor\` (not created by the current scaffold).

`resources\config.toml` is the reference sample:

- 8 items in 2 rows: up/down, CPU %, MEM %, GPU %, CPU temp, GPU temp,
  HDD/NVMe temp.
- 294 x 35 px, Segoe UI 8 pt, opacity 235.
- `theme = "dark"` plus dark-friendly per-item colors. The `theme` key is
  reserved for the future settings UI; the current `oxide-core` schema ignores
  it and the built-in `Theme::dark()` palette is the dark reference.
- All providers disabled (`hwinfo`, `lhm_http`, `helper`), so CPU temperature
  remains `—` unless a provider is enabled.
- `cpu_temp_source = "provider"` means "never invent a CPU temperature"; use
  `"auto"`, `"acpi"`, or `"off"` to change that policy.

## 5. Windows resources: manifest, icon, version info

The files in `resources\` are ready but **not embedded yet**. `Cargo.toml` has
no `build` script and the workspace must not be edited as part of this
packaging work. Until a build script is added, these resources are unused: the
executable keeps the toolchain default icon and has no custom version resource.

| File | Purpose |
|---|---|
| `resources\app.manifest` | `asInvoker`, `uiAccess="false"`, Per-Monitor V2 DPI, `longPathAware`, Common Controls v6, supportedOS entries |
| `resources\oxide.rc` | `RT_MANIFEST` (id 1), icon (id 1), `VS_VERSION_INFO` 0.1.0.0 |
| `resources\oxide.ico` | 16/32/48 px DIB entries and a 256 px PNG entry |
| `resources\config.toml` | portable sample configuration |

The manifest is security-relevant: do not change `asInvoker` to
`requireAdministrator`. Admin-only sensor work belongs in a separate helper
process, not in the UI.

The icon generator is reproducible:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\make-icon.ps1
```

It writes `resources\oxide.ico` and prints the ICONDIR header plus each entry
size. The current file starts with `00 00 01 00` and contains four entries.

### Enabling the resources in a future build script

This section is a plan, not something that has been applied. Adding it requires
editing `crates\oxide-app\Cargo.toml`, which is intentionally outside the scope
of the current change.

#### Option A: `winresource` (recommended for icon + manifest)

`winresource` generates the resource script from Cargo metadata and can embed
the icon, our manifest, and version strings.

In `crates\oxide-app\Cargo.toml`:

```toml
[package]
name = "oxide-app"
# ... existing fields ...
build = "build.rs"

[build-dependencies]
winresource = "0.1"
```

Create `crates\oxide-app\build.rs`:

```rust
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let icon = root.join("resources/oxide.ico");
    let manifest = root.join("resources/app.manifest");

    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", manifest.display());

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_string_lossy().as_ref());
    res.set_manifest_file(manifest.to_string_lossy().as_ref());
    res.set("CompanyName", "Oxide Monitor contributors");
    res.set("FileDescription", "Oxide Monitor taskbar system monitor");
    res.set("FileVersion", "0.1.0.0");
    res.set("InternalName", "oxide-monitor");
    res.set("OriginalFilename", "oxide-monitor.exe");
    res.set("ProductName", "Oxide Monitor");
    res.set("ProductVersion", "0.1.0.0");
    res.compile().expect("failed to compile Windows resources");
}
```

Then force a resource rebuild:

```powershell
cargo clean -p oxide-app
cargo build --release -p oxide-app
```

`set_icon` uses resource id 1, matching `resources\oxide.rc`.

#### Option B: compile `oxide.rc` with `embed-resource`

Use this if you want the exact `VS_VERSION_INFO` block from
`resources\oxide.rc` rather than `winresource`'s generated metadata.

In `crates\oxide-app\Cargo.toml`:

```toml
[package]
build = "build.rs"

[build-dependencies]
embed-resource = "3.0"
```

Create `crates\oxide-app\build.rs`:

```rust
fn main() {
    embed_resource::compile(
        "../../resources/oxide.rc",
        embed_resource::ParamsIncludeDirs(&["../../resources"]),
    )
    .manifest_required()
    .unwrap();
}
```

The include directory lets `rc.exe` resolve `app.manifest` and `oxide.ico`
from inside `oxide.rc`. Do not enable both options at once; two resource
objects with the same icon/manifest ids will conflict at link time.

After embedding, verify the result:

```powershell
# Version strings in Explorer / PowerShell
(Get-Item target\release\oxide-monitor.exe).VersionInfo |
    Format-List FileVersion, ProductVersion, ProductName, FileDescription

# Extract and inspect manifest resource id 1 with the SDK tool
& "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.0.26100.0\x64\mt.exe" `
    -inputresource:"target\release\oxide-monitor.exe;#1" `
    -out:"$env:TEMP\oxide-monitor.manifest"
Get-Content "$env:TEMP\oxide-monitor.manifest"

# Optional: dump the embedded manifest with sigcheck
sigcheck -m target\release\oxide-monitor.exe
```

To validate the resource script itself without linking an executable, run the
SDK resource compiler from the `resources` directory:

```powershell
Push-Location resources
& "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.0.26100.0\x64\rc.exe" `
    /nologo /fo "$env:TEMP\oxide.res" oxide.rc
Pop-Location
```

Adjust the SDK version in the path if 10.0.26100 is not installed. A successful
run creates `oxide.res` with exit code 0; the current files pass this check.

At runtime the application icon is resource id 1:

```rust
// Pseudocode: MAKEINTRESOURCEW(1)
let module = GetModuleHandleW(None)?;
let icon = LoadIconW(module, PCWSTR(1 as _))?;
```

## 6. Manifest details and verification

`resources\app.manifest` contains:

- `<requestedExecutionLevel level="asInvoker" uiAccess="false"/>` — matches
  ADR-02; the UI never elevates.
- `<dpiAwareness>PerMonitorV2</dpiAwareness>` with the legacy
  `<dpiAware>true/pm</dpiAware>` fallback.
- `<longPathAware>true</longPathAware>`.
- Common Controls `6.0.0.0` dependency for visual styles.
- `supportedOS` entries for Windows 7 through Windows 10/11.
- The application identity declares `processorArchitecture="amd64"`, matching
  the current x64 package. Add an architecture-specific manifest/identity if an
  arm64 build is introduced.

The DPI entries only take effect when the manifest is embedded. Before then,
test DPI behavior with the process started by a host that sets context, or
temporarily use an external manifest for local experiments. `WM_DPICHANGED`
handling in the UI is still required; the manifest alone does not implement
layout changes.

## 7. Troubleshooting

| Symptom | Fix |
|---|---|
| `link.exe` not found / `LINK : fatal error` | Install MSVC v143 x64 build tools and the Windows SDK, then reopen the terminal. |
| `error: toolchain 'stable-x86_64-pc-windows-gnu' is not installed` | `rustup default stable-x86_64-pc-windows-msvc`. |
| `rc.exe` not found when using `winresource` | Install the Windows SDK; `winresource` reads the SDK path from the registry. An explicit `set_toolkit_path` can help in unusual installations. |
| `RC2176 : old DIB in oxide.ico` | Regenerate the icon with `scripts\make-icon.ps1`; the generator writes 32-bpp DIB entries plus a PNG 256 px entry. |
| Icon does not change after rebuilding | `cargo clean -p oxide-app`, rebuild, and refresh the Explorer icon cache. |
| ZIP already exists / access denied | Close any Explorer window or process holding the ZIP, then rerun; the script deletes the old archive first. |
| SmartScreen warning | Expected for unsigned local builds. Sign the release binary/archive before distribution. |
| `config.toml` not picked up beside the exe | Ensure the file is named exactly `config.toml` and is in the executable's directory, or set `OXIDE_PORTABLE=1`. |
