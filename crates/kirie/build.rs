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
}
