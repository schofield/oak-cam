//! Gives this crate's own test binaries an rpath to the depthai-core install.
//!
//! `libdepthai-core` has install name `@rpath/libdepthai-core.dylib` (macOS) /
//! a bare soname (Linux) and lives outside `target/`, so `cargo test -p
//! depthai` would fail to load it. Link args from a library's build script
//! only reach that package's own linked targets (its test harness), never
//! downstream binaries; `oak-cam/build.rs` sets the rpath for the app.

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_DEPTHAI_LIBDIR");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        return;
    }
    if let Ok(libdir) = std::env::var("DEP_DEPTHAI_LIBDIR") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{libdir}");
    }
}
