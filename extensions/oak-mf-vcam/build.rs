//! MSVC asks for the four COM entry points to be exported PRIVATE (LNK4104);
//! rustc generates the export list itself and cannot mark them, so silence
//! that one linker warning for the cdylib.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-cdylib-link-arg=/IGNORE:4104");
    }
}
