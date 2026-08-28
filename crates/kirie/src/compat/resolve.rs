use std::path::{Path, PathBuf};

use kirie_formats::project::{Project, WallpaperType};

use crate::compat::args::{ParseError, WORKSHOP_APP_ID};
use crate::compat::steam;

const WORKSHOP_RELATIVE: &str = "workshop/content";

#[must_use]
pub fn workshop_dirs() -> Vec<PathBuf> {
    steam::steamapps_dirs(Path::new(WORKSHOP_RELATIVE).join(WORKSHOP_APP_ID))
}

pub fn translate_background(value: &str) -> Result<String, ParseError> {
    if value.contains('/') {
        return Ok(value.to_owned());
    }
    if std::env::var_os("HOME").is_none() && std::env::var_os("KIRIE_STEAM_LIBRARY").is_none() {
        return Err(fatal(
            "Cannot find home directory, please set the HOME environment variable",
        ));
    }
    for dir in workshop_dirs() {
        let candidate = dir.join(value);
        if candidate.is_dir() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(fatal(format!(
        "Cannot find workshop directory for steam app {WORKSHOP_APP_ID} and content {value}"
    )))
}

fn fatal(message: impl Into<String>) -> ParseError {
    ParseError {
        message: message.into(),
        doubled: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wallpaper {
    Video { media: PathBuf },
    Image { file: PathBuf },
    Scene { dir: PathBuf },
    Web { dir: PathBuf, file: String },
    Unsupported { kind: &'static str },
    Asset,
}

impl Wallpaper {
    #[must_use]
    pub fn unrunnable_reason(&self) -> Option<String> {
        match self {
            Wallpaper::Video { .. } | Wallpaper::Image { .. } | Wallpaper::Scene { .. } => None,
            #[cfg(any(feature = "web-cef", feature = "web-webview"))]
            Wallpaper::Web { .. } => None,
            #[cfg(not(any(feature = "web-cef", feature = "web-webview")))]
            Wallpaper::Web { .. } => Some(
                "web wallpapers need a web build (rebuild with --features web-cef or --features web-webview)"
                    .to_owned(),
            ),
            Wallpaper::Unsupported { kind: "application" } => {
                Some("Application wallpapers are not supported on this platform".to_owned())
            }
            Wallpaper::Unsupported { kind } => {
                Some(format!("{kind} wallpapers are not yet supported by kirie"))
            }
            Wallpaper::Asset => {
                Some("is a Wallpaper Engine asset (effect preset), not a renderable wallpaper".to_owned())
            }
        }
    }
}

#[must_use]
pub fn reason_for_kind(kind: &str) -> Option<String> {
    match kind {
        "scene" | "video" | "image" => None,
        "web" => Wallpaper::Web {
            dir: PathBuf::new(),
            file: String::new(),
        }
        .unrunnable_reason(),
        "asset" => Wallpaper::Asset.unrunnable_reason(),
        "application" => Wallpaper::Unsupported { kind: "application" }.unrunnable_reason(),
        _ => Some("Steam did not report this item's type".to_owned()),
    }
}

const WE_ASSETS_RELATIVE: &str = "common/wallpaper_engine/assets";

#[must_use]
pub fn we_assets_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KIRIE_WE_ASSETS") {
        let dir = PathBuf::from(dir);
        return dir.is_dir().then_some(dir);
    }
    steam::steamapps_dirs(WE_ASSETS_RELATIVE).into_iter().next()
}

#[must_use]
pub fn steam_assets_candidates() -> Vec<PathBuf> {
    steam::steamapps_candidates(WE_ASSETS_RELATIVE)
}

#[must_use]
pub fn we_assets_dir_or_warn() -> Option<PathBuf> {
    let dir = we_assets_dir();
    if dir.is_none() {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "Wallpaper Engine base assets not found — scenes that use builtin shaders \
                 (genericimage2/4, effects) will render BLANK. Install Wallpaper Engine via \
                 Steam, or set KIRIE_WE_ASSETS=/path/to/wallpaper_engine/assets. \
                 Run `kirie check <wallpaper>` for a full diagnosis."
            );
        }
    }
    dir
}

const VIDEO_EXTS: [&str; 6] = ["mp4", "webm", "mkv", "avi", "mov", "m4v"];
const IMAGE_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "bmp", "gif", "tex"];

pub fn classify(background: &str) -> Result<Wallpaper, ClassifyError> {
    let path = Path::new(background);
    if path.is_dir() {
        return classify_dir(path);
    }
    if path.is_file() {
        return Ok(classify_file(path));
    }
    Err(ClassifyError::NotFound {
        path: path.to_path_buf(),
    })
}

fn classify_dir(dir: &Path) -> Result<Wallpaper, ClassifyError> {
    let manifest = dir.join("project.json");
    let project = Project::from_path(&manifest).map_err(|source| ClassifyError::Project {
        path: manifest.clone(),
        reason: source.to_string(),
    })?;
    if project.is_asset() {
        return Ok(Wallpaper::Asset);
    }
    match project.resolved_type {
        WallpaperType::Video => Ok(Wallpaper::Video {
            media: dir.join(&project.file),
        }),
        WallpaperType::Image => Ok(Wallpaper::Image {
            file: dir.join(&project.file),
        }),
        WallpaperType::Scene => Ok(Wallpaper::Scene {
            dir: dir.to_path_buf(),
        }),
        WallpaperType::Web => Ok(Wallpaper::Web {
            dir: dir.to_path_buf(),
            file: project.file.clone(),
        }),
        WallpaperType::Application => Ok(Wallpaper::Unsupported { kind: "application" }),
    }
}

fn classify_file(file: &Path) -> Wallpaper {
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if VIDEO_EXTS.contains(&ext.as_str()) {
        Wallpaper::Video {
            media: file.to_path_buf(),
        }
    } else if IMAGE_EXTS.contains(&ext.as_str()) {
        Wallpaper::Image {
            file: file.to_path_buf(),
        }
    } else if matches!(ext.as_str(), "html" | "htm") {
        Wallpaper::Web {
            dir: file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(),
            file: file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    } else {
        Wallpaper::Unsupported { kind: "unknown" }
    }
}

#[must_use]
pub fn web_entry_url(dir: &Path, file: &str) -> String {
    let lower = file.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://") {
        return file.to_owned();
    }
    let path = dir.join(file);
    let abs = std::fs::canonicalize(&path).unwrap_or(path);
    file_url(&abs)
}

fn file_url(path: &Path) -> String {
    use std::path::Component;

    let mut url = String::from("file://");
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                url.push('/');
                for &b in seg.to_string_lossy().as_bytes() {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                        url.push(b as char);
                    } else {
                        url.push('%');
                        url.push(hex_digit(b >> 4));
                        url.push(hex_digit(b & 0x0f));
                    }
                }
            }
            Component::ParentDir => url.push_str("/.."),
            _ => {}
        }
    }
    if url == "file://" {
        url.push('/');
    }
    url
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClassifyError {
    #[error("background path does not exist: {path}")]
    NotFound { path: PathBuf },
    #[error("cannot load {path}: {reason}")]
    Project { path: PathBuf, reason: String },
}
