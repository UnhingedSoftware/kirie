//! `kirie check [wallpaper]` — a preflight doctor that verifies everything
//! needed to *build and run* a wallpaper, so a missing prerequisite fails
//! loudly with a fix instead of silently rendering a flat clear-color frame.
//!
//! Motivation: a scene references Wallpaper Engine's shared builtin shaders
//! (`genericimage2`, effect passes, …) which live in WE's install, not in the
//! per-item `scene.pkg`. If that asset directory is absent, the referenced
//! passes are skipped and the wallpaper composites to its clear color — a
//! blank/flat render with no error. This command surfaces exactly that (and
//! the other build/run prerequisites) as a checklist.
//!
//! Environment checks always run. Passing a wallpaper path additionally
//! resolves its type and, for scenes, performs a real headless build while
//! capturing the renderer's diagnostics (missing shaders, asset problems), so
//! the report reflects what an actual run would do.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::Headless;

/// One checklist line's verdict.
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

/// Accumulates checklist lines and the worst verdict seen.
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

    /// A continuation/hint line under the previous check (indented, no tag).
    fn hint(&self, text: &str) {
        println!("        {text}");
    }
}

/// Run the doctor. `path` is an optional wallpaper (workshop dir or a direct
/// media/scene file). Returns `Ok(true)` when everything required passed,
/// `Ok(false)` when a required check failed.
///
/// # Errors
/// Only for a truly unexpected I/O failure while classifying the path; missing
/// prerequisites are reported as `[FAIL]` lines, not `Err`.
pub fn run(path: Option<&Path>) -> Result<bool> {
    let mut r = Report::default();

    println!("kirie check — build/run prerequisites\n");
    println!("environment:");
    // One headless device for the whole run: the driver pipeline cache is
    // process-wide (a `OnceLock` bound to the device that created it), so a
    // second device would panic when the renderer reuses that cache. Report
    // the GPU from it and reuse it for the scene build.
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

/// A wgpu adapter must exist (hardware or software) to build any wallpaper.
/// Returns the headless device on success, for reuse by the scene build.
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

/// What Steam records about the Workshop library, read from its own files.
///
/// Answerable with the client closed, which is the point: a wallpaper daemon
/// starts at login and Steam usually is not up yet.
fn check_workshop(r: &mut Report) {
    let states = crate::compat::steam::workshop_item_states(crate::compat::args::WORKSHOP_APP_ID);
    if states.is_empty() {
        r.line(
            &Verdict::Ok,
            "workshop library",
            "Steam records no items for this app",
        );
        r.hint("subscribe to wallpapers in Steam, or browse from here: kirie workshop browse.");
        // Whether browsing works is *more* interesting on a machine with
        // nothing installed, not less — reporting it only for libraries that
        // already have wallpapers left a fresh install with no answer at all.
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

    // Both of these explain a wallpaper that is "missing" or looks wrong, and
    // neither was visible to kirie before.
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

/// Whether `kirie workshop` can reach Steam at all.
///
/// Browsing needs three things and fails differently on each: the helper
/// binary beside kirie, a running Steam client, and an account that owns
/// Wallpaper Engine. Steam enforces the last one, so the check is simply
/// whether the helper's `probe` comes back owning the app.
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

/// The shared WE builtin-assets directory: required by any scene that uses
/// builtin shaders/effects (the overwhelming majority).
fn check_we_assets(r: &mut Report) -> Option<std::path::PathBuf> {
    match resolve::we_assets_dir() {
        Some(dir) => {
            // Sanity: a canary builtin shader that virtually every scene uses.
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

/// Report which web backends this build can drive (feature-dependent) and
/// whether their out-of-process host binary ships beside the engine.
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
        // The host is this binary, re-executed (`kirie __webviewhost`), unless
        // an override or an older split install points elsewhere. What it
        // still needs from the system is WebKitGTK, which many distros do not
        // install by default and which ships under several parallel ABIs —
        // without this check that failure surfaces much later as an opaque
        // "webviewhost died during startup".
        // The host is a separate gtk-linked binary carried inside this one and
        // extracted on first use, so "is it there" is a property of the build,
        // not of the filesystem.
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

/// The WebKitGTK shared library the webview host can drive, if one is present.
///
/// Probes the standard library directories rather than `dlopen`ing (this crate
/// is `forbid(unsafe_code)`); the host itself loads whichever it finds. The
/// three ABIs are *parallel-installable different libraries*, not versions:
/// `4.1` (GTK3 + libsoup3) is today's mainstream, `4.0` (GTK3 + libsoup2)
/// survives on older LTS distros, and `6.0` is the GTK4 line. Ordered
/// most-preferred first.
#[cfg_attr(not(all(feature = "web-webview", not(feature = "web-cef"))), allow(dead_code))]
fn webkit_runtime() -> Option<String> {
    const SONAMES: &[&str] = &[
        "libwebkit2gtk-4.1.so.0",
        "libwebkit2gtk-4.0.so.37",
        "libwebkit2gtk-4.0.so",
    ];
    // Covers merged-/usr and Debian/Fedora multiarch layouts.
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

/// A copy-pasteable install command for the running distro.
///
/// Keyed off `/etc/os-release` `ID`/`ID_LIKE`. Deliberately a static table
/// rather than a package-manager query (`dnf provides`, `pacman -F`,
/// `apt-file`): those need an extra tool or a synced file database and can
/// touch the network, none of which belongs in a diagnostic. The soname is
/// always printed so the user can search their own repositories if their
/// distro isn't listed.
#[cfg_attr(not(all(feature = "web-webview", not(feature = "web-cef"))), allow(dead_code))]
fn webkit_install_hint() -> String {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    format!(
        "web wallpapers need WebKitGTK (libwebkit2gtk-4.1.so.0, or 4.0 on older distros): {}. \
         Scenes, videos and images work without it.",
        webkit_install_cmd(&os_release)
    )
}

/// The install command for the distro described by an `/etc/os-release` body.
///
/// Split out from [`webkit_install_hint`] so the mapping is testable without
/// the host's real `/etc/os-release`.
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

/// Classify the wallpaper and run type-specific prerequisite checks.
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

/// Headlessly build the scene and report any missing shaders/assets captured
/// from the renderer's own diagnostics — the exact failure mode behind a blank
/// scene.
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
        // The doctor builds at a fixed probe size, so fitting to "the output"
        // would only shrink the check's own canvas — it proves nothing here.
        fit_render_to_output: false,
    };

    // Capture the renderer's debug diagnostics during the build. A build that
    // logs `missing shader source` composites blank at runtime even though it
    // returns Ok (best-effort clear-color degradation, SPEC §V9).
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

    // Missing builtin shaders → passes skipped → blank render.
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

/// A video wallpaper: the media file must exist and open/decode.
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

/// An image wallpaper: the file must exist and decode.
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

/// A web wallpaper: the entry page must exist and a web backend must be built
/// in (runnability already reported by the environment web-backend check).
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

/// A `tracing_subscriber` writer that appends to a shared byte buffer, so the
/// scene build's diagnostics can be captured and scanned.
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

    /// The distro mapping keys off both `ID` and `ID_LIKE`, so derivatives
    /// (CachyOS, Pop!_OS, Rocky) resolve to their parent's package manager
    /// without needing their own entry.
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

    /// An unknown distro still gets actionable text rather than an empty hint.
    #[test]
    fn webkit_hint_has_a_generic_fallback() {
        let got = webkit_install_cmd("ID=someobscuredistro\n");
        assert!(got.to_lowercase().contains("webkitgtk"), "{got:?}");
    }
}
