//! `kirie list` — enumerate the Wallpaper Engine items installed on this
//! machine.
//!
//! Shells and panels otherwise have to rediscover all of this themselves:
//! where Steam put the workshop content, which folders are real items rather
//! than leftovers from an unsubscribe, what type each one is, and whether this
//! build can actually render it. kirie already answers every one of those
//! questions internally, so it answers them here too — as JSON for tooling
//! (`--json`) or a table for a human.
//!
//! The same listing is served over the control socket (`list`, see
//! docs/compat-socket.md), so a shell can use whichever it already has to hand.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::compat::resolve::{self, Wallpaper};

/// One installed workshop item.
#[derive(Debug, Clone)]
pub struct Item {
    /// Workshop id — the item directory's name.
    pub id: String,
    /// `project.json` title, or the id when the manifest has none.
    pub title: String,
    /// `scene` / `video` / `web` / `image` / `asset` / `application` /
    /// `unknown`.
    pub kind: &'static str,
    /// The item directory.
    pub dir: PathBuf,
    /// `preview.*` beside the manifest, when the item ships one.
    pub preview: Option<PathBuf>,
    /// Whether **this build** can render it: a web item needs a web backend,
    /// and application wallpapers are unsupported everywhere.
    ///
    /// False on an `asset` too, but for a different reason — an effect preset
    /// is a building block for wallpapers, not one itself. Consumers that
    /// report failures should read `kind` before calling this a problem.
    pub renderable: bool,
    /// Why not, when `renderable` is false.
    pub reason: Option<String>,
}

/// Every installed item across all Steam installation shapes, sorted by id.
///
/// `root` overrides discovery with a single workshop content directory (the
/// `--dir` flag); `None` probes the standard locations.
#[must_use]
pub fn scan(root: Option<&Path>) -> Vec<Item> {
    let roots: Vec<PathBuf> = match root {
        Some(dir) => vec![dir.to_path_buf()],
        None => resolve::workshop_dirs(),
    };

    let mut items: Vec<Item> = Vec::new();
    for dir in roots {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // No manifest, no wallpaper: Steam leaves the directory behind when
            // an item is unsubscribed, and a half-finished download has content
            // but no project.json yet.
            if !path.join("project.json").is_file() {
                continue;
            }
            if let Some(item) = describe(&path) {
                // A later Steam root never shadows an earlier one, matching the
                // priority order `translate_background` resolves a bare id with.
                if !items.iter().any(|existing| existing.id == item.id) {
                    items.push(item);
                }
            }
        }
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

/// Describe one item directory, or `None` when it is not a workshop item.
fn describe(dir: &Path) -> Option<Item> {
    let id = dir.file_name()?.to_string_lossy().into_owned();
    let project = kirie_formats::project::Project::from_path(dir.join("project.json")).ok();

    // Classification is what the engine itself would do with this path, so
    // `kind` and `renderable` cannot drift from what actually happens on load.
    let classified = resolve::classify(&dir.to_string_lossy()).ok();
    let kind = match (&classified, &project) {
        // An asset resolves by extension to a scene, so it has to be matched
        // ahead of one — calling it a scene would present an effect preset as
        // a wallpaper that failed to load.
        (Some(Wallpaper::Asset), _) => "asset",
        (Some(Wallpaper::Scene { .. }), _) => "scene",
        (Some(Wallpaper::Video { .. }), _) => "video",
        (Some(Wallpaper::Web { .. }), _) => "web",
        (Some(Wallpaper::Image { .. }), _) => "image",
        // Unsupported/asset items still have a declared type worth reporting.
        (_, Some(p)) => match p.resolved_type {
            kirie_formats::project::WallpaperType::Scene => "scene",
            kirie_formats::project::WallpaperType::Web => "web",
            kirie_formats::project::WallpaperType::Video => "video",
            kirie_formats::project::WallpaperType::Image => "image",
            kirie_formats::project::WallpaperType::Application => "application",
        },
        _ => "unknown",
    };

    let reason = match &classified {
        Some(w) => w.unrunnable_reason(),
        None => Some("not a recognised wallpaper".to_owned()),
    };

    Some(Item {
        title: project
            .as_ref()
            .map(|p| p.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| id.clone()),
        preview: preview_of(dir),
        renderable: reason.is_none(),
        reason,
        kind,
        dir: dir.to_path_buf(),
        id,
    })
}

/// The item's `preview.*` thumbnail, if it ships one.
fn preview_of(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut found: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .file_stem()
            .is_some_and(|s| s.eq_ignore_ascii_case("preview"))
        {
            // Prefer a still: an animated preview is a video/gif a consumer has
            // to decode, while every item that has one also has a static form
            // often enough to be worth the check.
            let is_still = path.extension().is_some_and(|e| {
                matches!(
                    e.to_string_lossy().to_lowercase().as_str(),
                    "jpg" | "jpeg" | "png"
                )
            });
            if is_still {
                return Some(path);
            }
            found.get_or_insert(path);
        }
    }
    found
}

/// The listing as a JSON array — the form a shell or panel consumes.
#[must_use]
pub fn to_json(items: &[Item]) -> String {
    let values: Vec<serde_json::Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "title": i.title,
                "type": i.kind,
                "dir": i.dir.to_string_lossy(),
                "preview": i.preview.as_ref().map(|p| p.to_string_lossy().into_owned()),
                "renderable": i.renderable,
                "reason": i.reason,
            })
        })
        .collect();
    serde_json::Value::Array(values).to_string()
}

/// Run `kirie list`.
///
/// # Errors
/// Never fails on an unreadable item — those are skipped — so this only
/// surfaces a write failure to stdout.
pub fn run(root: Option<&Path>, json: bool) -> Result<()> {
    let items = scan(root);

    if json {
        println!("{}", to_json(&items));
        return Ok(());
    }

    if items.is_empty() {
        println!("no Wallpaper Engine items found");
        println!("(subscribe to wallpapers in Steam, or pass --dir <workshop content dir>)");
        return Ok(());
    }

    let width = items.iter().map(|i| i.id.len()).max().unwrap_or(0);
    for item in &items {
        // Assets are non-renderable by design, so they get their own marker:
        // flagging them like a failure reads as "kirie cannot open this".
        let mark = match (item.renderable, item.kind) {
            (true, _) => ' ',
            (false, "asset") => '-',
            (false, _) => '!',
        };
        println!(
            "{mark} {:width$}  {:<11} {}",
            item.id,
            item.kind,
            item.title,
            width = width
        );
    }

    let assets = items.iter().filter(|i| i.kind == "asset").count();
    if assets > 0 {
        println!("\n{assets} item(s) marked - are assets (effects, presets) used to build");
        println!("wallpapers, not wallpapers themselves.");
    }
    let failed: Vec<&Item> = items
        .iter()
        .filter(|i| !i.renderable && i.kind != "asset")
        .collect();
    if !failed.is_empty() {
        println!("\n{} item(s) marked ! cannot be rendered:", failed.len());
        for item in failed {
            if let Some(reason) = &item.reason {
                println!("  {}: {reason}", item.id);
            }
        }
    }
    Ok(())
}
