//! Sets the runtime search path for `libdepthai-core`.
//!
//! A `-sys` crate's link args never reach the final executable, so the rpath
//! is set here. `DEP_DEPTHAI_LIBDIR` comes from depthai-sys's `cargo:libdir`
//! (its `links = "depthai"` key), and is only visible to direct dependents,
//! hence the direct depthai-sys dependency in Cargo.toml.
//!
//! - macOS: `@executable_path/../Frameworks` (app bundle), `@executable_path`,
//!   plus the absolute install libdir so `cargo run` works.
//! - Linux: `$ORIGIN` plus the absolute install libdir.
//! - Windows: nothing; the DLLs are copied next to the exe at packaging time.
//!
//! On Windows it also embeds `assets/icon/oak-cam.ico` as the exe icon
//! (winresource, a Windows-host build dependency).

fn main() {
    #[cfg(windows)]
    windows_icon();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DEP_DEPTHAI_LIBDIR");
    println!("cargo:rerun-if-env-changed=DEP_DEPTHAI_VERSION");
    // depthai-sys's `cargo:version` (from oak-flags.json), for --version.
    let version = std::env::var("DEP_DEPTHAI_VERSION").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=OAK_DEPTHAI_VERSION={version}");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let libdir = std::env::var("DEP_DEPTHAI_LIBDIR").ok();
    let rpath = |p: &str| println!("cargo:rustc-link-arg=-Wl,-rpath,{p}");
    match os.as_str() {
        "macos" => {
            rpath("@executable_path/../Frameworks");
            rpath("@executable_path");
        }
        "linux" => rpath("$ORIGIN"),
        _ => return,
    }
    if let Some(dir) = libdir {
        rpath(&dir);
    }
}

/// Embeds the exe icon when building on and for Windows.
#[cfg(windows)]
fn windows_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let ico = std::path::Path::new(&manifest).join("../../assets/icon/oak-cam.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy());
    res.compile().expect("cannot embed the Windows exe icon");
}
