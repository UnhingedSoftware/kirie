//! The `webview` web-wallpaper backend: the system `webkit2gtk`, 4.1 **or**
//! 4.0, resolved at run time.
//!
//! # Status: native-surface fallback only — off-screen is won't-fix (upstream)
//!
//! The CEF backend ([`crate::cef`]) renders **off-screen**: Chromium paints
//! into a CPU pixel buffer that kirie uploads to a wgpu texture and composites
//! like any other wallpaper (the [`crate::backend::WebBackend`] trait is shaped
//! around that — `new(url, size)` + `latest_frame()`).
//!
//! webkit2gtk cannot do that, and this is an upstream limitation on Linux —
//! not pending kirie work. Evidence, from the wry 0.55.1 source this backend
//! was originally written against (`~/.cargo/registry/src/…/wry-0.55.1/`):
//!
//! * `src/webkitgtk/mod.rs` — the complete Linux `WebView` API surface is
//!   `eval`, `load_url(_with_headers)`, `load_html`, `reload`, `bounds` /
//!   `set_bounds`, `set_visible`, `focus`, `zoom`, `print`, devtools, cookies,
//!   `clear_all_browsing_data`. **No** snapshot, paint-callback, or pixel
//!   read-back API of any kind; rendering goes straight to the GTK widget via
//!   webkit's own GL context.
//! * `src/lib.rs` (`WebViewExtUnix::webview`) hands out the underlying
//!   `webkit2gtk::WebView`, and the `webkit2gtk` crate (2.0.2,
//!   `src/auto/web_view.rs`) does expose `WebViewExt::snapshot` —
//!   `webkit_web_view_get_snapshot`. That is an **asynchronous, GTK-main-loop
//!   -bound screenshot** call (WebKit re-renders the page into a fresh
//!   `cairo::Surface` per invocation): unusable as a 60 fps per-frame OSR
//!   feed, still `!Send`, and still requiring a realized native window.
//! * True off-screen WebKit on Linux is **WPE WebKit** (`wpewebkit`), a
//!   different engine build with a different C API — not something the
//!   webkit2gtk shared object exposes at all.
//!
//! Implementing the off-screen, `Send`, frame-publishing `WebBackend` trait on
//! top of this API is therefore impossible; the aspiration to make the
//! `webview` feature composite through the wgpu presentation layer is closed
//! as **won't-fix**. Use the `cef` feature for that.
//!
//! # Rendering model — native surface
//!
//! This backend uses the only model webkit2gtk supports for a full-surface
//! HTML wallpaper: **it fills the background window directly.** The host
//! (`kirie-webviewhost`) creates the background surface — a GTK window
//! promoted to the compositor's background layer via `gtk-layer-shell` — and
//! hands the backend the `gtk::Container` inside it; the `WebKitWebView`
//! becomes a child widget and renders into it. There is no wgpu compositing
//! step and no per-frame `latest_frame()` upload for web wallpapers on this
//! backend. This matches how WE web wallpapers are authored: standalone
//! full-surface `index.html` documents.
//!
//! Everything else is kept identical to the CEF backend: the same WE JS bridge
//! shims ([`crate::shim`]) are injected, audio is pushed to
//! `wallpaperRegisterAudioListener`, properties flow through
//! `wallpaperPropertyListener`, and audio can be muted (via JavaScript, since
//! webkit2gtk exposes no native mute).
//!
//! # Build requirement — GTK 3 only
//!
//! The `webview` feature links GTK 3 and `gtk-layer-shell` at compile time
//! (both are present on every target distro) and **`dlopen`s webkit at run
//! time** (see `webkit_sys`). That is a deliberate departure from `wry`,
//! whose `webkit2gtk-sys` dependency hard-requires the `webkit2gtk-4.1`
//! pkg-config module and so bakes a `DT_NEEDED` on `libwebkit2gtk-4.1.so.0`
//! into the binary: such a build cannot start at all on the LTS distros that
//! ship only `webkit2gtk-4.0` (Ubuntu 20.04, Debian 11), even though the two
//! expose the same GTK3 C API and differ only in which libsoup they link.
//! Resolving the handful of entry points this backend needs at run time gives
//! one binary that serves both, and drops webkit from the *build* requirements
//! entirely. The default build still enables no web feature at all.
//!
//! # Runtime preconditions (host responsibility)
//!
//! A `WebKitWebView` is a GTK widget: `gtk::init()` must have run on the
//! current thread before one is created, and the object is `!Send` — it must
//! be created and driven on the one GTK main thread. So the host must, on the
//! thread that owns the web wallpaper: call `gtk::init()` once, create the
//! background GTK/gtk-layer-shell window, then build the [`WebviewBackend`]
//! against its container and run the GTK main loop (that loop — not
//! [`WebviewBackend::tick`] — is what actually paints webkit). These are host
//! misconfigurations, not page input, so this crate documents rather than
//! defends against them (SPEC V9 concerns malformed *wallpaper* input).

mod backend;
pub mod host;
mod webkit_sys;

pub use backend::WebviewBackend;

/// Turn a filesystem path to a wallpaper entry page into a `file://` URL.
///
/// webkit2gtk resolves the page's relative asset references against the URL's
/// directory, so the entry page must be given as an absolute `file://` URL
/// (docs/subsystems-misc.md §3.4). The path is percent-encoded per RFC 3986
/// for the bytes that are unsafe in a URL path, leaving `/` as the separator.
/// The path is **not** canonicalized here (that is I/O the caller may have
/// already done); pass an absolute path.
#[must_use]
pub fn file_url(path: &std::path::Path) -> String {
    use std::path::Component;

    let mut url = String::from("file://");
    for comp in path.components() {
        match comp {
            Component::RootDir => { /* leading '/' emitted below per-segment */ }
            Component::Prefix(_) => { /* Windows prefixes: not a target platform */ }
            Component::CurDir => {}
            Component::ParentDir => {
                url.push('/');
                url.push_str("..");
            }
            Component::Normal(seg) => {
                url.push('/');
                encode_segment(&seg.to_string_lossy(), &mut url);
            }
        }
    }
    if url == "file://" {
        // Root path.
        url.push('/');
    }
    url
}

/// Percent-encode one path segment into `out`, leaving RFC 3986 unreserved
/// characters (and a small safe set) untouched.
fn encode_segment(seg: &str, out: &mut String) {
    for &b in seg.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn encodes_spaces_and_keeps_slashes() {
        let url = file_url(Path::new("/home/a b/My Wallpaper/index.html"));
        assert_eq!(url, "file:///home/a%20b/My%20Wallpaper/index.html");
    }

    #[test]
    fn keeps_unreserved() {
        let url = file_url(Path::new("/a-b_c.d~e/index.html"));
        assert_eq!(url, "file:///a-b_c.d~e/index.html");
    }
}
