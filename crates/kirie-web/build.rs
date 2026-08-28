fn main() {
    if std::env::var_os("CARGO_FEATURE_CEF").is_some() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
    if std::env::var_os("CARGO_FEATURE_WEBVIEW").is_some() {
        println!(
            "cargo:warning=kirie-web `webview` feature: webkit2gtk cannot render \
             off-screen (upstream limitation, won't-fix); this backend paints a native \
             surface only — use `--features cef` (kirie: `web-cef`) for composited web \
             wallpapers"
        );
    }
}
