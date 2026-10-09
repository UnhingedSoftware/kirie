use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kirie_formats::project::{Project, WallpaperType};
use kirie_pack::{Kind, Manifest, Package, Provenance, Summary, WALLPAPER_ENGINE_ENTRY};

/// `kirie pack <dir>`: turn a folder holding `kirie.json` into a package.
pub fn run(dir: &Path, output: Option<PathBuf>) -> Result<()> {
    let output = output.unwrap_or_else(|| {
        let name = dir
            .file_name()
            .map_or_else(|| "wallpaper".into(), |n| n.to_string_lossy().into_owned());
        PathBuf::from(format!("{name}.{}", kirie_pack::EXTENSION))
    });
    let summary = write_package(&output, |file| kirie_pack::pack_dir(dir, file))
        .with_context(|| format!("cannot pack {}", dir.display()))?;
    report(&output, &summary);
    Ok(())
}

/// `kirie convert <dir>`: repack a Wallpaper Engine item, as it is, into a
/// package marked as converted. kirie plays it the way it plays the item's
/// own folder; the package is for this machine and must not be published.
pub fn convert(dir: &Path, output: Option<PathBuf>) -> Result<()> {
    let manifest = we_manifest(dir)?;
    let output =
        output.unwrap_or_else(|| PathBuf::from(format!("{}.{}", manifest.id, kirie_pack::EXTENSION)));
    let summary = write_package(&output, |file| kirie_pack::pack_folder(dir, manifest, &[], file))
        .with_context(|| format!("cannot convert {}", dir.display()))?;
    report(&output, &summary);
    Ok(())
}

fn report(output: &Path, summary: &Summary) {
    println!(
        "{}: {} entries, {} bytes",
        output.display(),
        summary.entries,
        summary.bytes
    );
}

/// The manifest for a Wallpaper Engine item, from its `project.json`.
fn we_manifest(dir: &Path) -> Result<Manifest> {
    let project_path = dir.join(WALLPAPER_ENGINE_ENTRY);
    let project = Project::from_path(&project_path)
        .with_context(|| format!("cannot read {}", project_path.display()))?;
    if project.is_asset() {
        bail!("{} is a Wallpaper Engine asset, not a wallpaper", dir.display());
    }
    if project.resolved_type == WallpaperType::Application {
        bail!(
            "{} is an application wallpaper, which kirie cannot run",
            dir.display()
        );
    }
    let source_id = project
        .workshopid
        .as_ref()
        .map(ToString::to_string)
        .filter(|id| !id.trim().is_empty())
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .context("the item has no Workshop id and its folder has no name")?;
    let text = |key: &str| {
        project
            .extra
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let title = Some(project.title.trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| source_id.clone());
    // A preview the item names but does not hold would make the package
    // invalid; it is only a picture, so it is dropped instead.
    let preview =
        text("preview").filter(|p| !p.starts_with('.') && !p.contains('\\') && dir.join(p).is_file());
    let tags = project
        .extra
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(Manifest {
        id: package_id(&source_id),
        title,
        description: text("description").unwrap_or_default(),
        author: String::new(),
        kind: Kind::WallpaperEngine,
        entry: WALLPAPER_ENGINE_ENTRY.to_owned(),
        preview,
        tags,
        mature: text("contentrating").is_some_and(|r| r.eq_ignore_ascii_case("mature")),
        properties: Vec::new(),
        min_kirie: None,
        provenance: Provenance::Converted {
            source: "wallpaper_engine".to_owned(),
            source_id,
        },
    })
}

/// A package id for a converted item: `we-` and the Workshop id, with
/// anything a package id may not hold turned into `-`.
fn package_id(source_id: &str) -> String {
    let mut id: String = format!("we-{source_id}")
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    id.truncate(128);
    id
}

/// Write a package beside `output` and rename it into place, so a failed
/// run never leaves a half-written package where a good one was. The
/// partial file is hidden (so packing into the source folder skips it),
/// named after this process (so two runs to the same output do not share
/// it), and created exclusively (so a link planted at that path is not
/// followed).
fn write_package(
    output: &Path,
    fill: impl FnOnce(&mut File) -> Result<Summary, kirie_pack::PackError>,
) -> Result<Summary> {
    let file_name = output
        .file_name()
        .map_or_else(|| "wallpaper".into(), |n| n.to_string_lossy().into_owned());
    let partial = output.with_file_name(format!(".{file_name}.{}.partial", std::process::id()));
    let mut file =
        File::create_new(&partial).with_context(|| format!("cannot create {}", partial.display()))?;
    let summary = match fill(&mut file) {
        Ok(summary) => summary,
        Err(err) => {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(err.into());
        }
    };
    let synced = file.sync_all();
    drop(file);
    let finished = synced
        .and_then(|()| std::fs::rename(&partial, output))
        .with_context(|| format!("cannot write {}", output.display()));
    if finished.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    finished?;
    Ok(summary)
}

/// A string from a package, safe to print: control characters (terminal
/// escape sequences among them) come out escaped rather than acted on.
fn shown(text: &str) -> String {
    text.chars().flat_map(char::escape_debug).collect()
}

/// `kirie pack --inspect <file>`: print a package's manifest and entries,
/// and check every entry's hash.
pub fn inspect(path: &Path) -> Result<()> {
    let mut package = Package::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let m = package.manifest();
    println!(
        "{} ({}), {} package",
        shown(&m.title),
        shown(&m.id),
        m.kind.as_str()
    );
    if !m.author.is_empty() {
        println!("by {}", shown(&m.author));
    }
    println!("entry: {}", shown(&m.entry));
    if let Some(preview) = &m.preview {
        println!("preview: {}", shown(preview));
    }
    if !m.tags.is_empty() {
        println!("tags: {}", shown(&m.tags.join(", ")));
    }
    if m.mature {
        println!("mature: yes");
    }
    for p in &m.properties {
        println!("property: {} ({})", shown(&p.key), shown(&p.label));
    }
    if let Some(v) = &m.min_kirie {
        println!("needs kirie {} or newer", shown(v));
    }
    match &m.provenance {
        Provenance::Original => {}
        Provenance::Converted { source, source_id } => {
            println!(
                "converted from {} item {}; stays on this machine, cannot be published",
                shown(source),
                shown(source_id)
            );
        }
        Provenance::ConvertedOwn { source, source_id } => {
            println!(
                "converted from the publisher's own {} item {}",
                shown(source),
                shown(source_id)
            );
        }
    }
    println!();
    for e in package.entries() {
        println!("{:>12}  {:<4}  {}", e.len, shown(&e.compression), shown(&e.path));
    }
    package.verify().context("the package failed its hash check")?;
    println!("\nall {} entries match their hashes", package.entries().len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_ids_for_converted_items_are_valid() {
        assert_eq!(package_id("1388331347"), "we-1388331347");
        assert_eq!(package_id("My Item!"), "we-my-item-");
        assert_eq!(package_id(&"x".repeat(300)).len(), 128);
    }

    #[test]
    fn a_wallpaper_engine_item_converts_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("kirie-convert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::write(
            dir.join("project.json"),
            br#"{"title":"Rainy street","file":"scene.json","type":"scene","workshopid":"1388331347",
                "preview":"preview.gif","tags":["Anime"],"contentrating":"Everyone",
                "description":"rain"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("scene.json"), b"{}").unwrap();
        std::fs::write(dir.join("materials/rain.json"), b"{}").unwrap();
        std::fs::write(dir.join("preview.gif"), b"GIF89a").unwrap();
        let out = dir.with_extension("kpk");
        let _ = std::fs::remove_file(&out);

        convert(&dir, Some(out.clone())).unwrap();

        let mut p = Package::open(&out).unwrap();
        let m = p.manifest().clone();
        assert_eq!(m.id, "we-1388331347");
        assert_eq!(m.title, "Rainy street");
        assert_eq!(m.kind, Kind::WallpaperEngine);
        assert_eq!(m.preview.as_deref(), Some("preview.gif"));
        assert_eq!(m.tags, ["Anime"]);
        assert!(!m.provenance.publishable());
        assert_eq!(p.read("materials/rain.json").unwrap(), b"{}");
        p.verify().unwrap();

        std::fs::remove_file(&out).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn assets_are_not_converted() {
        let dir = std::env::temp_dir().join(format!("kirie-convert-asset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            br#"{"title":"Preset","file":"preset.json","type":"preset","category":"Asset"}"#,
        )
        .unwrap();
        assert!(we_manifest(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
