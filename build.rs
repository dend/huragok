// Compile the structured-exception guard (csrc/seh.c) with the MSVC toolchain.
// Rust has no __try/__except, so we keep the SEH frame in a tiny C shim and call
// into it from src/seh.rs to survive access violations from wrong reflected offsets.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn main() {
    cc::Build::new()
        .file("csrc/seh.c")
        .compile("huragok_seh");
    println!("cargo:rerun-if-changed=csrc/seh.c");

    // Version derived from git: <commit-count>.<short-hash>[-dirty]. Deterministic across
    // machines, advances with history, needs no local state. Re-runs when HEAD or the
    // index moves (commit / checkout / stage) so it stays current automatically.
    let count = git(&["rev-list", "--count", "HEAD"]).unwrap_or_else(|| "0".into());
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "nogit".into());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let version = format!("{count}.{hash}{}", if dirty { "-dirty" } else { "" });
    println!("cargo:rustc-env=HURAGOK_BUILD={version}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    // VERSIONINFO resource: FileVersion/ProductVersion come from CARGO_PKG_VERSION,
    // so Cargo.toml is the single source of truth for the release version.
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "Huragok");
    res.set("FileDescription", "Huragok in-process gameplay customization engine");
    res.set("OriginalFilename", "huragok.dll");
    res.set("InternalName", "huragok");
    res.set("ProductVersion", &format!("{pkg_version}+{version}"));
    res.compile().expect("failed to embed VERSIONINFO resource");
}
