//! `kirie-webviewhost` — standalone entry point for the webview wallpaper
//! host.
//!
//! The host itself is [`kirie_web::webview::host`]; this binary exists so the
//! engine can still spawn a sibling process (older installs, and the
//! `KIRIE_WEBVIEWHOST` override). A `kirie` built with `web-webview` embeds
//! the same code and re-executes itself instead, so a normal install is one
//! file.

fn main() {
    kirie_web::webview::host::run();
}
