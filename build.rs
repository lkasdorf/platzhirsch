//! Embeds the icon and the version information in `platzhirsch.exe`, for the Windows
//! MSVC target only; on other targets this does nothing. `bench.exe` and the test
//! programs get neither. Needs the Windows SDK's resource compiler `rc.exe`.
use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/platzhirsch.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // A build script runs on the host, so the target is read from cargo, not `cfg!`.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/platzhirsch.ico")
        // English (United States).
        .set_language(0x0409)
        .set("ProductName", "Platzhirsch")
        .set("FileDescription", env!("CARGO_PKG_DESCRIPTION"))
        .set("FileVersion", env!("CARGO_PKG_VERSION"))
        .set("ProductVersion", env!("CARGO_PKG_VERSION"))
        .set("LegalCopyright", "Copyright (c) 2026 Leon Kasdorf")
        .set("OriginalFilename", "platzhirsch.exe")
        .set("Comments", "MIT License");
    // Linked through `cargo:rustc-link-arg-bin=platzhirsch=...`: only this binary.
    if let Err(e) = res.compile_for("platzhirsch") {
        panic!("embedding the icon and version information failed: {e}");
    }
}
