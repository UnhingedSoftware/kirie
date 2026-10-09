//! What a web wallpaper's view is pointed at, and what runs in it first.
//!
//! Kept apart from the WebView2 host so it compiles, and is tested, on every
//! platform: none of it touches a browser.

use std::path::{Path, PathBuf};

use crate::shim;

/// The made-up host a wallpaper's own folder is served under. `.invalid` is
/// reserved and never resolves, so it can only ever mean this mapping.
pub const FOLDER_HOST: &str = "wallpaper.kirie.invalid";

/// What to show: a wallpaper's folder and its entry page, or an address.
#[derive(Debug, Clone)]
pub enum PageSource {
    Folder { dir: PathBuf, file: String },
    Url(String),
}

impl PageSource {
    /// Classify what a `project.json` names as its `file`: a web address stays
    /// one, anything else is a page inside the wallpaper's folder.
    ///
    /// A `file://` address is not a web address: taking it as one would let a
    /// downloaded wallpaper open any file on the machine in its view.
    #[must_use]
    pub fn of(dir: &Path, file: &str) -> Self {
        let lower = file.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Self::Url(file.to_owned());
        }
        Self::Folder {
            dir: dir.to_path_buf(),
            file: file.to_owned(),
        }
    }

    /// The one string a web backend is handed for this page.
    ///
    /// An address passes through unchanged. A folder page is written as
    /// `kirie-folder:<dir>?<file>`, percent-escaped, so a backend that serves
    /// the folder under [`FOLDER_HOST`] knows the whole folder and not just the
    /// entry page's directory. [`Self::from_arg`] reads it back.
    #[must_use]
    pub fn to_arg(&self) -> String {
        match self {
            Self::Url(url) => url.clone(),
            Self::Folder { dir, file } => {
                format!(
                    "{FOLDER_ARG}{}?{}",
                    escape_segment(&dir.to_string_lossy()),
                    escape_segment(file)
                )
            }
        }
    }

    /// Reads back what [`Self::to_arg`] wrote. Anything else is an address.
    #[must_use]
    pub fn from_arg(arg: &str) -> Self {
        let folder = arg.strip_prefix(FOLDER_ARG).and_then(|rest| {
            let (dir, file) = rest.split_once('?')?;
            Some(Self::Folder {
                dir: PathBuf::from(unescape(dir)?),
                file: unescape(file)?,
            })
        });
        folder.unwrap_or_else(|| Self::Url(arg.to_owned()))
    }

    /// The file inside the wallpaper's folder that a request for `url` names,
    /// or `None` when it names nothing there.
    ///
    /// Only paths under [`FOLDER_HOST`] resolve. `..`, drive prefixes and
    /// anything that lands outside the folder once symlinks are followed are
    /// refused, so a page can read its own folder and nothing else.
    #[must_use]
    pub fn resolve(&self, url: &str) -> Option<PathBuf> {
        let Self::Folder { dir, .. } = self else {
            return None;
        };
        let rest = url.strip_prefix("https://")?.strip_prefix(FOLDER_HOST)?;
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        if !path.is_empty() && !path.starts_with('/') {
            return None;
        }
        let mut target = dir.clone();
        for raw in path.split('/').filter(|part| !part.is_empty()) {
            let part = unescape(raw)?;
            if part == "."
                || part == ".."
                || part.contains(['/', '\\', '\0'])
                || (cfg!(windows) && part.contains(':'))
            {
                return None;
            }
            target.push(part);
        }
        let root = dir.canonicalize().ok()?;
        let target = target.canonicalize().ok()?;
        (target.starts_with(&root) && target.is_file()).then_some(target)
    }

    /// The address the view is sent to.
    ///
    /// A wallpaper's folder is served over a mapped `https://` host rather
    /// than opened as `file://`. Pages fetch their own JSON, images and audio
    /// with relative URLs, and Chromium refuses most of that from a `file://`
    /// origin; from a mapped host it is an ordinary same-origin request.
    #[must_use]
    pub fn address(&self) -> String {
        match self {
            Self::Url(url) => url.clone(),
            Self::Folder { file, .. } => {
                let mut url = format!("https://{FOLDER_HOST}");
                for part in file.split(['/', '\\']).filter(|part| !part.is_empty()) {
                    url.push('/');
                    url.push_str(&escape_segment(part));
                }
                if url.ends_with(FOLDER_HOST) {
                    url.push('/');
                }
                url
            }
        }
    }
}

const FOLDER_ARG: &str = "kirie-folder:";

fn unescape(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The `Content-Type` to serve a wallpaper's file with, from its extension.
#[must_use]
pub fn mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "glsl" | "frag" | "vert" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn escape_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for &b in segment.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What runs in every document before the page's own scripts: the Wallpaper
/// Engine bridge, the wallpaper's properties, and the volume.
#[must_use]
pub fn init_script(properties: &str, volume: f32) -> String {
    let level = if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let mut script = String::from(shim::BRIDGE_INIT);
    script.push('\n');
    if properties != "{}" && !properties.trim().is_empty() {
        script.push_str(&shim::apply_user_properties_call(properties));
        script.push('\n');
    }
    script.push_str(&format!(
        "(function(){{\
var v={level};\
var apply=function(){{document.querySelectorAll('video,audio').forEach(function(el){{el.muted=v<=0;el.volume=v;}});}};\
document.addEventListener('DOMContentLoaded',apply);\
try{{new MutationObserver(apply).observe(document.documentElement||document,{{childList:true,subtree:true}});}}catch(e){{}}\
}})();"
    ));
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_page_is_served_from_the_mapped_host() {
        let source = PageSource::of(Path::new(r"C:\walls\123"), "index.html");
        assert_eq!(source.address(), "https://wallpaper.kirie.invalid/index.html");
    }

    #[test]
    fn nested_pages_and_odd_names_are_escaped() {
        let source = PageSource::of(Path::new(r"C:\walls\123"), r"web\my page#1.html");
        assert_eq!(
            source.address(),
            "https://wallpaper.kirie.invalid/web/my%20page%231.html"
        );
    }

    #[test]
    fn an_address_is_left_alone() {
        let source = PageSource::of(Path::new(r"C:\walls\123"), "https://example.com/wall");
        assert_eq!(source.address(), "https://example.com/wall");
    }

    #[test]
    fn a_file_address_stays_inside_the_folder() {
        let source = PageSource::of(Path::new("/walls/123"), "file:///etc/passwd");
        assert!(matches!(source, PageSource::Folder { .. }));
        assert!(source.address().starts_with("https://wallpaper.kirie.invalid/"));
    }

    #[test]
    fn a_folder_page_round_trips_through_its_arg() {
        let source = PageSource::of(Path::new("/walls/my wall?#%"), "web/index 1.html");
        let arg = source.to_arg();
        assert!(arg.starts_with("kirie-folder:"));
        let back = PageSource::from_arg(&arg);
        let PageSource::Folder { dir, file } = back else {
            panic!("not a folder: {arg}");
        };
        assert_eq!(dir, Path::new("/walls/my wall?#%"));
        assert_eq!(file, "web/index 1.html");
        assert!(matches!(
            PageSource::from_arg("https://example.com"),
            PageSource::Url(_)
        ));
        assert!(matches!(
            PageSource::from_arg("kirie-folder:%zz?a"),
            PageSource::Url(_)
        ));
    }

    #[test]
    fn requests_resolve_inside_the_folder_only() {
        let base = std::env::temp_dir().join(format!("kirie-page-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("item");
        std::fs::create_dir_all(dir.join("sub")).expect("scratch");
        std::fs::write(dir.join("index.html"), "x").expect("scratch");
        std::fs::write(dir.join("sub/a b.json"), "x").expect("scratch");
        std::fs::write(base.join("secret"), "x").expect("scratch");
        let source = PageSource::of(&dir, "index.html");
        let host = format!("https://{FOLDER_HOST}");

        let found = source.resolve(&format!("{host}/sub/a%20b.json?v=1#top"));
        assert_eq!(found, dir.join("sub/a b.json").canonicalize().ok());
        assert!(source.resolve(&format!("{host}/index.html")).is_some());

        for escape in [
            "/../secret",
            "/sub/../../secret",
            "/%2e%2e/secret",
            "/sub%2f..%2f..%2fsecret",
            "/..%5csecret",
            "/sub",
            "/missing.html",
        ] {
            assert_eq!(source.resolve(&format!("{host}{escape}")), None, "{escape}");
        }
        assert_eq!(source.resolve("https://example.com/index.html"), None);
        assert_eq!(source.resolve(&format!("{host}.evil.com/index.html")), None);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("secret"), dir.join("link")).expect("symlink");
            assert_eq!(source.resolve(&format!("{host}/link")), None);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_init_script_carries_properties_and_volume() {
        let script = init_script(r#"{"a":{"value":1}}"#, 0.5);
        assert!(script.contains("__wpBridge"));
        assert!(script.contains(r#"__wpApplyProps({"a":{"value":1}})"#));
        assert!(script.contains("var v=0.5"));
        assert!(!init_script("{}", 1.0).contains("__wpApplyProps("));
    }
}
