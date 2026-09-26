use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kirie_pack::{Package, Provenance};

/// `kirie pack <dir>`: turn a folder holding `kirie.json` into a package.
pub fn run(dir: &Path, output: Option<PathBuf>) -> Result<()> {
    let output = output.unwrap_or_else(|| {
        let name = dir
            .file_name()
            .map_or_else(|| "wallpaper".into(), |n| n.to_string_lossy().into_owned());
        PathBuf::from(format!("{name}.{}", kirie_pack::EXTENSION))
    });
    // Write beside the destination and rename, so a failed pack never
    // leaves a half-written package where a good one was.
    let partial = output.with_extension(format!("{}.partial", kirie_pack::EXTENSION));
    let mut file =
        std::fs::File::create(&partial).with_context(|| format!("cannot create {}", partial.display()))?;
    let summary = match kirie_pack::pack_dir(dir, &mut file) {
        Ok(summary) => summary,
        Err(err) => {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(err).with_context(|| format!("cannot pack {}", dir.display()));
        }
    };
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, &output).with_context(|| format!("cannot write {}", output.display()))?;
    println!(
        "{}: {} entries, {} bytes",
        output.display(),
        summary.entries,
        summary.bytes
    );
    Ok(())
}

/// `kirie pack --inspect <file>`: print a package's manifest and entries,
/// and check every entry's hash.
pub fn inspect(path: &Path) -> Result<()> {
    let mut package = Package::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let m = package.manifest();
    println!("{} ({}), {} package", m.title, m.id, m.kind.as_str());
    if !m.author.is_empty() {
        println!("by {}", m.author);
    }
    println!("entry: {}", m.entry);
    if let Some(preview) = &m.preview {
        println!("preview: {preview}");
    }
    if !m.tags.is_empty() {
        println!("tags: {}", m.tags.join(", "));
    }
    if m.mature {
        println!("mature: yes");
    }
    for p in &m.properties {
        println!("property: {} ({})", p.key, p.label);
    }
    if let Some(v) = &m.min_kirie {
        println!("needs kirie {v} or newer");
    }
    match &m.provenance {
        Provenance::Original => {}
        Provenance::Converted { source, source_id } => {
            println!("converted from {source} item {source_id}; stays on this machine, cannot be published");
        }
        Provenance::ConvertedOwn { source, source_id } => {
            println!("converted from the publisher's own {source} item {source_id}");
        }
    }
    println!();
    for e in package.entries() {
        println!("{:>12}  {:<4}  {}", e.len, e.compression, e.path);
    }
    package.verify().context("the package failed its hash check")?;
    println!("\nall {} entries match their hashes", package.entries().len());
    Ok(())
}
