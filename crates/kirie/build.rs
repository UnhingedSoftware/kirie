//! Embeds the webview host binary, when one is offered.
//!
//! The webview host needs gtk3 + gtk-layer-shell (webkit has no off-screen
//! path, so it presents itself on a layer surface). Compiling it *into* the
//! engine made the install one file, but it also put gtk in the engine's
//! `NEEDED`, and the dynamic linker maps that whole chain — gtk, gdk, cairo,
//! pango, fontconfig, gdk-pixbuf, epoxy, X11 — at exec, for every wallpaper
//! including scene-only ones that never open a browser. Measured on a scene
//! render: 186 MB peak and 562 mappings without it, 196 MB and 827 with.
//!
//! So the host goes back to being its own binary, and the engine carries it as
//! bytes instead of as linkage: one file to ship, no gtk mapped until a web
//! wallpaper actually runs.
//!
//! Set `KIRIE_EMBED_WEBVIEWHOST=/path/to/kirie-webviewhost` to embed one. With
//! it unset the blob is empty and the engine falls back to a sibling binary or
//! `KIRIE_WEBVIEWHOST`, which is what local dev builds do.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=KIRIE_EMBED_WEBVIEWHOST");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let blob = out_dir.join("webviewhost.bin");

    match std::env::var_os("KIRIE_EMBED_WEBVIEWHOST") {
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            println!("cargo:rerun-if-changed={}", path.display());
            let bytes = std::fs::read(&path).unwrap_or_else(|err| {
                panic!(
                    "KIRIE_EMBED_WEBVIEWHOST points at {} which cannot be read: {err}",
                    path.display()
                )
            });
            assert!(
                !bytes.is_empty(),
                "KIRIE_EMBED_WEBVIEWHOST points at an empty file: {}",
                path.display()
            );
            std::fs::write(&blob, &bytes).expect("write embedded host blob");
        }
        // No host offered: an empty blob, which the runtime reads as "not
        // embedded" and falls back on.
        _ => std::fs::write(&blob, []).expect("write empty host blob"),
    }

    println!("cargo:rustc-env=KIRIE_WEBVIEWHOST_BLOB={}", blob.display());
}
