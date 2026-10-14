//! Compiles the C++ shim (`csrc/oak_dai.cpp`) against an installed
//! depthai-core and links the shared library.
//!
//! The install prefix is `DEPTHAI_PREFIX` if set, else
//! `$OAK_DEPTHAI_ROOT/current`, else the default root's `current`:
//! `~/.cache/oak-cam/depthai/current` on macOS and Linux (a symlink made by
//! `scripts/depthai/build.sh`) and `%LOCALAPPDATA%\oak-cam\depthai\current`
//! on Windows (a junction made by `scripts/depthai/fetch-windows.ps1`). The
//! prefix must contain `oak-flags.json`, written by
//! `scripts/depthai/probe.sh` (the Windows fetch script runs the same
//! probe), which lists the
//! include directories and compile definitions `find_package(depthai)` would
//! hand a `CMake` consumer (depthai's headers pull in `nlohmann/json`, spdlog,
//! libnop and friends from vcpkg-private directories).
//!
//! Emits `cargo:prefix=<prefix>` and `cargo:libdir=<dir>` so the binary crate
//! can set an rpath (`DEP_DEPTHAI_LIBDIR`); a `-sys` crate's own link args do
//! not reach the final executable.

use std::env;
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct Flags {
    version: String,
    core_library: String,
    include_dirs: Vec<String>,
    defines: Vec<String>,
}

fn main() {
    println!("cargo:rerun-if-env-changed=DEPTHAI_PREFIX");
    println!("cargo:rerun-if-env-changed=OAK_DEPTHAI_ROOT");
    println!("cargo:rerun-if-changed=csrc/oak_dai.h");
    println!("cargo:rerun-if-changed=csrc/oak_dai.cpp");
    println!("cargo:rerun-if-changed=build.rs");

    let prefix = find_prefix();
    let flags_path = prefix.join("oak-flags.json");
    let flags: Flags = match std::fs::read_to_string(&flags_path) {
        Ok(text) => serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{}: not valid oak-flags.json: {e}", flags_path.display())),
        Err(e) => panic!(
            "\n\ndepthai-sys: cannot read {}: {e}\n\
             Install depthai-core first:\n    {}\n\
             or point DEPTHAI_PREFIX at an install that has oak-flags.json \
             (scripts/depthai/probe.sh --prefix <dir>).\n",
            flags_path.display(),
            install_hint()
        ),
    };
    println!("cargo:rerun-if-changed={}", flags_path.display());

    let core = PathBuf::from(&flags.core_library);
    let libdir = core
        .parent()
        .map_or_else(|| prefix.join("lib"), Path::to_path_buf);
    let libname = link_name(&core);

    let target = env::var("TARGET").unwrap_or_default();
    let msvc = target.contains("msvc");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("csrc/oak_dai.cpp")
        .include("csrc")
        .warnings(false);
    // depthai's headers are third-party: include them as system headers, the
    // way CMake treats an imported target's include dirs. Without this, clang
    // >= 19 rejects libnop's `value_.template Construct(...)` with the
    // default-error -Wmissing-template-arg-list-after-template-kw.
    let prefix_include = prefix.join("include");
    for dir in
        std::iter::once(prefix_include.as_path()).chain(flags.include_dirs.iter().map(Path::new))
    {
        if msvc {
            build.include(dir);
        } else {
            build.flag("-isystem").flag(dir.as_os_str());
        }
    }
    for def in &flags.defines {
        match def.split_once('=') {
            Some((k, v)) => build.define(k, Some(v)),
            None => build.define(def, None),
        };
    }
    if msvc {
        build
            .flag("/EHsc")
            .flag("/Zc:__cplusplus")
            .flag("/permissive-");
    } else {
        build.flag("-fexceptions").flag("-fvisibility=hidden");
    }
    if target.contains("apple") {
        build.flag("-mmacosx-version-min=14.0");
    }
    build.compile("oak_dai");

    println!("cargo:rustc-link-search=native={}", libdir.display());
    if msvc {
        // The probe reports the DLL (`bin/depthai-core.dll`); MSVC links
        // against its import library in `lib/`.
        println!(
            "cargo:rustc-link-search=native={}",
            prefix.join("lib").display()
        );
    }
    println!("cargo:rustc-link-lib=dylib={libname}");
    if target.contains("windows") {
        // The shim calls inet_pton (telling an IP selector from a device
        // id); oak_dai.cpp also has `#pragma comment(lib, "Ws2_32.lib")`.
        println!("cargo:rustc-link-lib=ws2_32");
    }
    if !msvc {
        // Only reaches this package's own test harness (the dylib's install
        // name is @rpath/...); downstream binaries set their own rpath.
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", libdir.display());
    }
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
    } else if target.contains("linux") {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:prefix={}", prefix.display());
    println!("cargo:libdir={}", libdir.display());
    println!("cargo:core_library={}", core.display());
    println!("cargo:version={}", flags.version);
}

fn find_prefix() -> PathBuf {
    if let Ok(p) = env::var("DEPTHAI_PREFIX") {
        let p = PathBuf::from(p);
        assert!(
            p.is_dir(),
            "DEPTHAI_PREFIX={} is not a directory",
            p.display()
        );
        return canonical(&p);
    }
    let root = env::var("OAK_DEPTHAI_ROOT").map_or_else(|_| default_root(), PathBuf::from);
    let current = root.join("current");
    assert!(
        current.is_dir(),
        "\n\ndepthai-sys: no depthai-core install at {}.\n\
         Run {}, or set DEPTHAI_PREFIX (or OAK_DEPTHAI_ROOT).\n",
        current.display(),
        install_hint()
    );
    canonical(&current)
}

/// The default install root. Build scripts run on the host, so `cfg!` here
/// describes the machine the install lives on.
fn default_root() -> PathBuf {
    if cfg!(windows) {
        let local = env::var("LOCALAPPDATA").expect("LOCALAPPDATA must be set on Windows");
        PathBuf::from(local).join("oak-cam").join("depthai")
    } else {
        let home = env::var("HOME").expect("HOME must be set");
        PathBuf::from(home).join(".cache/oak-cam/depthai")
    }
}

/// The command that installs depthai-core on this host.
fn install_hint() -> &'static str {
    if cfg!(windows) {
        r"powershell -ExecutionPolicy Bypass -File scripts\depthai\fetch-windows.ps1"
    } else {
        "scripts/depthai/build.sh"
    }
}

fn canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // Windows canonical paths are verbatim (`\\?\C:\...`), which cl.exe and
    // link.exe do not accept in /I and /LIBPATH: drop the prefix for plain
    // drive paths.
    match c.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => c,
    }
}

/// The `-l` name for the core library: `libdepthai-core.dylib` -> `depthai-core`,
/// `depthai-core.lib` -> `depthai-core`.
fn link_name(core: &Path) -> String {
    let stem = core
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("depthai-core");
    // `libdepthai-core.3.10.0` style stems keep the version after the first dot.
    let stem = stem.split('.').next().unwrap_or(stem);
    stem.strip_prefix("lib").unwrap_or(stem).to_string()
}
