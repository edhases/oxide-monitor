//! Embed the `requireAdministrator` manifest into the helper **binary only**.
//!
//! A normal `winres` build script would link the resource into every target of the
//! package, including the lib test harness, making `cargo test` demand elevation.
//! `cargo:rustc-link-arg-bins` scopes the `.res` file to the `oxide-pawnio-host`
//! binary, so tests stay medium-integrity.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/oxide-pawnio-host.manifest");

    // PawnIO is an x64 Windows driver; skip the ARM64 workspace check.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64")
    {
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("oxide-pawnio-host.manifest");
    let manifest = manifest.display().to_string().replace('\\', "/");

    let rc_source = out.join("oxide-pawnio-host.rc");
    std::fs::write(&rc_source, format!("1 24 \"{manifest}\"\n")).expect("write rc");

    let res = out.join("oxide-pawnio-host.res");
    let rc = find_rc()
        .expect("rc.exe not found; install the Windows SDK or build without the x64 host manifest");
    let status = Command::new(rc)
        .arg("/nologo")
        .arg(format!("/fo{}", res.display()))
        .arg(&rc_source)
        .status()
        .expect("run rc.exe");
    assert!(status.success(), "rc.exe failed to compile the manifest");

    println!("cargo:rustc-link-arg-bins={}", res.display());
}

fn find_rc() -> Option<PathBuf> {
    let mut roots = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(key) {
            roots.push(
                PathBuf::from(base)
                    .join("Windows Kits")
                    .join("10")
                    .join("bin"),
            );
        }
    }

    let mut versions = Vec::new();
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let rc = entry.path().join("x64").join("rc.exe");
                if rc.is_file() {
                    versions.push((entry.file_name(), rc));
                }
            }
        }
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().next().map(|(_, rc)| rc)
}
