use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::compat::resolve::{self, Wallpaper};

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub dir: PathBuf,
    pub preview: Option<PathBuf>,
    pub renderable: bool,
    pub reason: Option<String>,
    pub update_available: bool,
}

#[must_use]
pub fn scan(root: Option<&Path>) -> Vec<Item> {
    let roots: Vec<PathBuf> = match root {
        Some(dir) => vec![dir.to_path_buf()],
        None => resolve::workshop_dirs(),
    };

    let pending: std::collections::HashSet<String> =
        crate::compat::steam::workshop_item_states(crate::compat::args::WORKSHOP_APP_ID)
            .into_iter()
            .filter(|state| state.update_available)
            .map(|state| state.id)
            .collect();

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
            if !path.join("project.json").is_file() {
                continue;
            }
            if let Some(mut item) = describe(&path) {
                item.update_available = pending.contains(&item.id);
                if !items.iter().any(|existing| existing.id == item.id) {
                    items.push(item);
                }
            }
        }
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

pub(crate) fn describe(dir: &Path) -> Option<Item> {
    let id = dir.file_name()?.to_string_lossy().into_owned();
    let project = kirie_formats::project::Project::from_path(dir.join("project.json")).ok();

    let classified = resolve::classify(&dir.to_string_lossy()).ok();
    let kind = match (&classified, &project) {
        (Some(Wallpaper::Asset), _) => "asset",
        (Some(Wallpaper::Scene { .. }), _) => "scene",
        (Some(Wallpaper::Video { .. }), _) => "video",
        (Some(Wallpaper::Web { .. }), _) => "web",
        (Some(Wallpaper::Image { .. }), _) => "image",
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
        update_available: false,
    })
}

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
                "update_available": i.update_available,
            })
        })
        .collect();
    serde_json::Value::Array(values).to_string()
}

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
