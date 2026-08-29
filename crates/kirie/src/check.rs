use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::Headless;

enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl Verdict {
    fn tag(&self) -> &'static str {
        match self {
            Verdict::Ok => "[ ok ]",
            Verdict::Warn => "[warn]",
            Verdict::Fail => "[FAIL]",
        }
    }
}

#[derive(Default)]
struct Report {
    any_fail: bool,
}

impl Report {
    fn line(&mut self, v: &Verdict, label: &str, detail: &str) {
        if matches!(v, Verdict::Fail) {
            self.any_fail = true;
        }
        if detail.is_empty() {
            println!("{} {label}", v.tag());
        } else {
            println!("{} {label}: {detail}", v.tag());
        }
    }

    fn hint(&self, text: &str) {
        println!("        {text}");
    }
}

pub fn run(path: Option<&Path>) -> Result<bool> {
    let mut r = Report::default();

    println!("kirie check — build/run prerequisites\n");
    println!("environment:");
    let gpu = check_gpu(&mut r);
    let assets = check_we_assets(&mut r);
    check_workshop(&mut r);
    check_web_backends(&mut r);

    if let Some(path) = path {
        println!("\nwallpaper: {}", path.display());
        check_wallpaper(&mut r, path, assets.as_deref(), gpu.as_ref());
    } else {
        println!("\n(no wallpaper path given — pass one to validate a specific item,");
        println!(" e.g. `kirie check ~/…/steamapps/workshop/content/431960/<id>`)");
    }

    println!();
    if r.any_fail {
        println!("result: FAILED — see the [FAIL] lines above.");
    } else {
        println!("result: ok — all required checks passed.");
    }
    Ok(!r.any_fail)
}

fn check_gpu(r: &mut Report) -> Option<Headless> {
    match Headless::new() {
        Ok(gpu) => {
            let info = gpu.adapter.get_info();
            let kind = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => "discrete GPU",
                wgpu::DeviceType::IntegratedGpu => "integrated GPU",
                wgpu::DeviceType::VirtualGpu => "virtual GPU",
                wgpu::DeviceType::Cpu => "software (CPU)",
                wgpu::DeviceType::Other => "other",
            };
            r.line(
                &Verdict::Ok,
                "GPU adapter",
                &format!("{} ({}, {kind})", info.name, info.backend),
            );
            Some(gpu)
        }
        Err(e) => {
            r.line(&Verdict::Fail, "GPU adapter", "none available");
            r.hint(&format!("wgpu found no Vulkan or GL adapter: {e}"));
            r.hint("install a Vulkan driver, or mesa's software fallback (vulkan-swrast).");
            None
        }
    }
}

fn check_workshop(r: &mut Report) {
    let states = crate::compat::steam::workshop_item_states(crate::compat::args::WORKSHOP_APP_ID);
    if states.is_empty() {
        r.line(
            &Verdict::Ok,
            "workshop library",
            "Steam records no items for this app",
        );
        r.hint("subscribe to wallpapers in Steam, or browse from here: kirie workshop browse.");
        check_workshop_browse(r);
        return;
    }

    let installed = states.iter().filter(|s| s.installed).count();
    let waiting = states.iter().filter(|s| s.subscribed && !s.installed).count();
    let stale = states.iter().filter(|s| s.update_available).count();

    r.line(
        &Verdict::Ok,
        "workshop library",
        &format!("{installed} installed of {} subscribed", states.len()),
    );

    if waiting > 0 {
        r.line(
            &Verdict::Warn,
            "workshop downloads",
            &format!("{waiting} subscribed but not downloaded yet"),
        );
        r.hint("Steam has not fetched them; they will not appear in `kirie list` until it does.");
    }
    if stale > 0 {
        r.line(
            &Verdict::Warn,
            "workshop updates",
            &format!("{stale} installed item(s) are out of date"),
        );
        r.hint("Steam has a newer manifest than the copy on disk; it updates them when it next runs.");
    }

    check_workshop_browse(r);
}

fn check_workshop_browse(r: &mut Report) {
    let Some(helper) = crate::workshop::helper_path() else {
        r.line(&Verdict::Warn, "workshop browse", "kirie-steam-helper NOT FOUND");
        r.hint("`kirie workshop` needs the helper that ships beside kirie; set KIRIE_STEAM_HELPER to point at it.");
        return;
    };

    match crate::workshop::probe() {
        Ok(true) => r.line(&Verdict::Ok, "workshop browse", &helper.display().to_string()),
        Ok(false) => {
            r.line(
                &Verdict::Warn,
                "workshop browse",
                "Steam is running, but this account does not own Wallpaper Engine",
            );
            r.hint("browsing and subscribing go through Steam, which only answers for an account that owns the app.");
        }
        Err(why) => {
            r.line(&Verdict::Warn, "workshop browse", &why.to_string());
            r.hint("`kirie list` and everything else keep working; only browsing the Workshop needs Steam running.");
        }
    }
}

fn check_we_assets(r: &mut Report) -> Option<std::path::PathBuf> {
    match resolve::we_assets_dir() {
        Some(dir) => {
            let canary = dir.join("shaders").join("genericimage2.frag");
            if canary.is_file() {
                r.line(&Verdict::Ok, "WE base assets", &dir.display().to_string());
            } else {
                r.line(
                    &Verdict::Warn,
                    "WE base assets",
                    &format!("{} (present but genericimage2 shader missing)", dir.display()),
                );
                r.hint("the directory exists but looks incomplete — scenes may still render blank.");
            }
            Some(dir)
        }
        None => {
            r.line(&Verdict::Fail, "WE base assets", "NOT FOUND");
            r.hint("scenes that use builtin shaders will render BLANK (flat clear color) without these.");
            r.hint("fix: install Wallpaper Engine via Steam, or point kirie at an assets dir:");
            r.hint("     export KIRIE_WE_ASSETS=/path/to/wallpaper_engine/assets");
            r.hint("probed (none existed):");
            for p in resolve::steam_assets_candidates() {
                r.hint(&format!("  {}", p.display()));
            }
            None
        }
    }
}

#[allow(unused_variables)]
fn check_web_backends(r: &mut Report) {
    let beside = |name: &str| -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join(name).is_file()))
            .unwrap_or(false)
    };
    let _ = &beside;

    #[cfg(feature = "web-cef")]
    {
        if beside("kirie-webhost") {
            r.line(&Verdict::Ok, "web backend (CEF)", "kirie-webhost present");
        } else {
            r.line(
                &Verdict::Warn,
                "web backend (CEF)",
                "kirie-webhost NOT beside the engine",
            );
            r.hint("web wallpapers need kirie-webhost next to the kirie binary (or KIRIE_WEBHOST).");
        }
    }
    #[cfg(all(feature = "web-webview", not(feature = "web-cef")))]
    {
        const HOST_BLOB: &[u8] = include_bytes!(env!("KIRIE_WEBVIEWHOST_BLOB"));
        let host = match std::env::var_os("KIRIE_WEBVIEWHOST") {
            Some(path) => format!("host {}", std::path::Path::new(&path).display()),
            None if !HOST_BLOB.is_empty() => {
                format!("host carried in this binary ({} KB)", HOST_BLOB.len() / 1024)
            }
            None if cfg!(feature = "web-webview-inproc") => "host built in".to_owned(),
            None => "no host embedded (set KIRIE_WEBVIEWHOST)".to_owned(),
        };
        match webkit_runtime() {
            Some(lib) => r.line(&Verdict::Ok, "web backend (webview)", &format!("{host}, {lib}")),
            None => {
                r.line(
                    &Verdict::Warn,
                    "web backend (webview)",
                    &format!("{host}, but no WebKitGTK runtime found"),
                );
                r.hint(&webkit_install_hint());
            }
        }
    }
    #[cfg(not(any(feature = "web-cef", feature = "web-webview")))]
    {
        r.line(
            &Verdict::Ok,
            "web backend",
            "not built in (scenes/video/images only; rebuild with --features web-cef for web)",
        );
    }
}

#[cfg_attr(not(all(feature = "web-webview", not(feature = "web-cef"))), allow(dead_code))]
fn webkit_runtime() -> Option<String> {
    const SONAMES: &[&str] = &[
        "libwebkit2gtk-4.1.so.0",
        "libwebkit2gtk-4.0.so.37",
        "libwebkit2gtk-4.0.so",
    ];
    const DIRS: &[&str] = &[
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/local/lib",
    ];
    for so in SONAMES {
        for dir in DIRS {
            if std::path::Path::new(dir).join(so).exists() {
                return Some((*so).to_owned());
            }
        }
    }
    None
}

#[cfg_attr(not(all(feature = "web-webview", not(feature = "web-cef"))), allow(dead_code))]
fn webkit_install_hint() -> String {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    format!(
        "web wallpapers need WebKitGTK (libwebkit2gtk-4.1.so.0, or 4.0 on older distros): {}. \
         Scenes, videos and images work without it.",
        webkit_install_cmd(&os_release)
    )
}

#[cfg_attr(not(all(feature = "web-webview", not(feature = "web-cef"))), allow(dead_code))]
fn webkit_install_cmd(os_release: &str) -> &'static str {
    let field = |key: &str| -> String {
        os_release
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or("")
            .trim_matches(['"', '\''].as_ref())
            .to_ascii_lowercase()
    };
    let ids = format!("{} {}", field("ID="), field("ID_LIKE="));
    let has = |needles: &[&str]| needles.iter().any(|n| ids.contains(n));

    if has(&["arch", "cachyos", "manjaro", "endeavouros"]) {
        "sudo pacman -S webkit2gtk-4.1"
    } else if has(&["ubuntu", "debian", "pop", "mint", "raspbian"]) {
        "sudo apt install libwebkit2gtk-4.1-0   (older releases: libwebkit2gtk-4.0-37)"
    } else if has(&["fedora", "rhel", "centos", "rocky", "alma"]) {
        "sudo dnf install webkit2gtk4.1"
    } else if has(&["opensuse", "suse"]) {
        "sudo zypper install libwebkit2gtk-4_1-0"
    } else if has(&["alpine"]) {
        "sudo apk add webkit2gtk-4.1"
    } else if has(&["void"]) {
        "sudo xbps-install -S webkit2gtk"
    } else if has(&["gentoo"]) {
        "sudo emerge net-libs/webkit-gtk"
    } else if has(&["nixos"]) {
        "add pkgs.webkitgtk_4_1 to your configuration"
    } else {
        "install your distro's WebKitGTK runtime package"
    }
}

fn check_wallpaper(r: &mut Report, path: &Path, assets: Option<&Path>, gpu: Option<&Headless>) {
    let wp = match resolve::classify(&path.to_string_lossy()) {
        Ok(wp) => wp,
        Err(e) => {
            r.line(&Verdict::Fail, "type", &format!("cannot classify: {e}"));
            return;
        }
    };
    match &wp {
        Wallpaper::Scene { dir } => {
            r.line(&Verdict::Ok, "type", "scene (kirie-render)");
            check_scene(r, dir, assets, gpu);
        }
        Wallpaper::Video { media } => {
            r.line(&Verdict::Ok, "type", "video (kirie-video)");
            check_video(r, media);
        }
        Wallpaper::Image { file } => {
            r.line(&Verdict::Ok, "type", "image (kirie-render)");
            check_image(r, file);
        }
        Wallpaper::Web { dir, file } => {
            r.line(&Verdict::Ok, "type", "web (kirie-web)");
            check_web(r, dir, file);
        }
        Wallpaper::Unsupported { kind } => {
            r.line(&Verdict::Fail, "type", &format!("unsupported ({kind})"));
        }
        Wallpaper::Asset => {
            r.line(
                &Verdict::Fail,
                "type",
                "Wallpaper Engine asset (effect preset), not a wallpaper",
            );
        }
    }
}

fn check_scene(r: &mut Report, dir: &Path, assets: Option<&Path>, gpu: Option<&Headless>) {
    if !dir.join("scene.pkg").is_file() {
        r.line(&Verdict::Fail, "scene.pkg", "missing");
        return;
    }
    r.line(&Verdict::Ok, "scene.pkg", "present");

    let Some(gpu) = gpu else {
        r.line(
            &Verdict::Warn,
            "scene build",
            "skipped (no GPU adapter — see above)",
        );
        return;
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let render_target = kirie_platform::RenderTarget {
        device: &gpu.device,
        queue: &gpu.queue,
        format,
        output_name: "check",
        size: (1280, 720),
    };
    let options = kirie_render::SceneOptions {
        render_scale: 1.0,
        scaling: kirie_render::ScalingMode::Default,
        clamp: kirie_render::ClampMode::Clamp,
        disable_parallax: false,
        fit_render_to_output: false,
        only_objects: Vec::new(),
        skip_objects: Vec::new(),
    };

    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufMakeWriter(buf.clone()))
        .with_ansi(false)
        .with_target(true)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "kirie_render=debug,kirie_shader=debug",
        ))
        .finish();

    let build = tracing::subscriber::with_default(subscriber, || {
        kirie_render::load_workshop_scene(&render_target, dir, assets, options, None, &[])
    });

    let log = String::from_utf8_lossy(&buf.lock().map(|g| g.clone()).unwrap_or_default()).into_owned();

    if let Err(e) = build {
        r.line(&Verdict::Fail, "scene build", &e.to_string());
        return;
    }

    let missing: Vec<String> = log
        .lines()
        .filter(|l| l.contains("missing shader source"))
        .filter_map(|l| {
            l.split("shader=")
                .nth(1)
                .map(|s| s.split_whitespace().next().unwrap_or(s).to_owned())
        })
        .collect();
    let mut missing_uniq: Vec<String> = missing;
    missing_uniq.sort();
    missing_uniq.dedup();

    let asset_problems = log.lines().filter(|l| l.contains("scene asset problem")).count();

    if !missing_uniq.is_empty() {
        r.line(
            &Verdict::Fail,
            "scene shaders",
            &format!("{} missing — scene will render blank", missing_uniq.len()),
        );
        r.hint(&format!("missing: {}", missing_uniq.join(", ")));
        if assets.is_none() {
            r.hint(
                "cause: WE base assets not found (see the [FAIL] above) — install WE or set KIRIE_WE_ASSETS.",
            );
        } else {
            r.hint("these shaders were not in the pkg nor the WE assets dir — the assets install may be incomplete.");
        }
    } else {
        r.line(&Verdict::Ok, "scene shaders", "all referenced passes resolved");
    }

    if asset_problems > 0 {
        r.line(
            &Verdict::Warn,
            "scene assets",
            &format!("{asset_problems} asset problem(s) reported (missing textures/materials)"),
        );
    }
}

fn check_video(r: &mut Report, media: &Path) {
    if !media.is_file() {
        r.line(
            &Verdict::Fail,
            "media file",
            &format!("missing: {}", media.display()),
        );
        return;
    }
    let options = kirie_video::VideoOptions {
        enable_audio: false,
        ..kirie_video::VideoOptions::default()
    };
    match kirie_video::VideoPlayer::open(media, options) {
        Ok(_) => r.line(&Verdict::Ok, "video decode", "opens and decodes"),
        Err(e) => {
            r.line(&Verdict::Fail, "video decode", &format!("cannot open: {e}"));
            r.hint("this build's ffmpeg may lack the needed codec, or the file is unreadable.");
        }
    }
}

fn check_image(r: &mut Report, file: &Path) {
    if !file.is_file() {
        r.line(
            &Verdict::Fail,
            "image file",
            &format!("missing: {}", file.display()),
        );
        return;
    }
    match kirie_render::ImageContent::from_path(file) {
        Ok(_) => r.line(&Verdict::Ok, "image decode", "decodes"),
        Err(e) => r.line(&Verdict::Fail, "image decode", &format!("cannot decode: {e}")),
    }
}

fn check_web(r: &mut Report, dir: &Path, file: &str) {
    if file.starts_with("http://") || file.starts_with("https://") {
        r.line(&Verdict::Ok, "web entry", &format!("remote URL: {file}"));
    } else {
        let entry = dir.join(file);
        if entry.is_file() {
            r.line(&Verdict::Ok, "web entry", &entry.display().to_string());
        } else {
            r.line(
                &Verdict::Fail,
                "web entry",
                &format!("missing entry page: {}", entry.display()),
            );
        }
    }
    #[cfg(not(any(feature = "web-cef", feature = "web-webview")))]
    {
        r.line(
            &Verdict::Fail,
            "web backend",
            "this build cannot run web wallpapers",
        );
        r.hint(
            "rebuild with --features web-cef (bundled Chromium) or --features web-webview (system webkit).",
        );
    }
}

#[derive(Clone)]
struct BufMakeWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for BufMakeWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut b) = self.0.lock() {
            b.extend_from_slice(data);
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufMakeWriter {
    type Writer = BufMakeWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::webkit_install_cmd;

    #[test]
    fn webkit_hint_matches_distro_family() {
        let cases = [
            ("ID=cachyos\nID_LIKE=arch\n", "pacman"),
            ("ID=arch\n", "pacman"),
            ("ID=ubuntu\nID_LIKE=debian\n", "apt"),
            ("ID=pop\nID_LIKE=\"ubuntu debian\"\n", "apt"),
            ("ID=fedora\n", "dnf"),
            ("ID=rocky\nID_LIKE=\"rhel centos fedora\"\n", "dnf"),
            ("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n", "zypper"),
            ("ID=alpine\n", "apk"),
            ("ID=void\n", "xbps"),
            ("ID=gentoo\n", "emerge"),
        ];
        for (os_release, want) in cases {
            let got = webkit_install_cmd(os_release);
            assert!(got.contains(want), "{os_release:?} -> {got:?}, expected {want:?}");
        }
    }

    #[test]
    fn webkit_hint_has_a_generic_fallback() {
        let got = webkit_install_cmd("ID=someobscuredistro\n");
        assert!(got.to_lowercase().contains("webkitgtk"), "{got:?}");
    }
}
