use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=KIRIE_EMBED_WEBVIEWHOST");
    println!("cargo:rerun-if-env-changed=KIRIE_RELEASE_TAG");

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
        _ => std::fs::write(&blob, []).expect("write empty host blob"),
    }

    println!("cargo:rustc-env=KIRIE_WEBVIEWHOST_BLOB={}", blob.display());

    link_media_foundation();
    embed_manifest();
}

// vcpkg's ffmpeg builds avcodec with the Media Foundation encoder, so avcodec.lib
// carries references to three COM interface GUIDs that live in the Windows SDK's
// own libraries rather than in any ffmpeg object: IID_IMFTransform and
// IID_IMFMediaEventGenerator in mfuuid, IID_ICodecAPI in strmiids. ffmpeg-sys-next
// names ole32, secur32, ws2_32, bcrypt and user32 on its vcpkg path and stops
// there, so without this the final link fails with three unresolved externals.
// Both are SDK libraries that ship with the MSVC toolchain, so there is nothing
// to install; they are simply not asked for.
fn link_media_foundation() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-lib=mfuuid");
        println!("cargo:rustc-link-lib=strmiids");
    }
}

// kirie.exe says which Windows versions it knows about (kirie.exe.manifest has
// why). The release build is MSVC, whose linker embeds a manifest given these
// two flags. A build without one still runs; it only loses the layered window
// on a raised desktop and draws straight into Progman instead.
fn embed_manifest() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !(windows && msvc) {
        return;
    }
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("kirie.exe.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bin=kirie=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=kirie=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
