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
    #[must_use]
    pub fn of(dir: &Path, file: &str) -> Self {
        let lower = file.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://") {
            return Self::Url(file.to_owned());
        }
        Self::Folder {
            dir: dir.to_path_buf(),
            file: file.to_owned(),
        }
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
    fn the_init_script_carries_properties_and_volume() {
        let script = init_script(r#"{"a":{"value":1}}"#, 0.5);
        assert!(script.contains("__wpBridge"));
        assert!(script.contains(r#"__wpApplyProps({"a":{"value":1}})"#));
        assert!(script.contains("var v=0.5"));
        assert!(!init_script("{}", 1.0).contains("__wpApplyProps("));
    }
}
