//! XPLM symbols are provided by X-Plane at load time. Windows uses `raw-dylib`
//! imports (see `src/ffi.rs`); macOS needs to be told to leave them undefined.
//! Linux shared objects allow undefined symbols by default.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
