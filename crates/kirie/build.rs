//! Build script: bake an `$ORIGIN` rpath into the `kirie` binary for `web-cef`
//! builds.
//!
//! The CEF backend links `libcef.so` as a load-time (`DT_NEEDED`) dependency,
//! and the rest of the CEF runtime (`icudtl.dat`, `*.pak`, `locales/`, the
//! `kirie-cef-helper` subprocess) lives beside the binary. Without an rpath the
//! dynamic loader can't find `libcef.so` unless `LD_LIBRARY_PATH` is set, so a
//! plainly-installed binary fails to start for *every* wallpaper. An `$ORIGIN`
//! runpath makes the install self-contained: drop the binary + its runtime in
//! one directory and it just works, no environment needed (docs §3.2).
//!
//! Gated on the feature so the default (no-web) build — which has no `libcef`
//! and is the portable single binary — is byte-for-byte unaffected.

fn main() {
    if std::env::var_os("CARGO_FEATURE_WEB_CEF").is_some() {
        // `$ORIGIN` must reach the linker literally; cargo/rustc exec the linker
        // without a shell, so no expansion happens here (the loader expands it at
        // runtime to the binary's own directory).
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
