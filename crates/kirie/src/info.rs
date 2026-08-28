use std::path::Path;

use anyhow::{Context, Result, bail};
use kirie_formats::pkg::OwnedPkg;
use kirie_formats::project::{Project, PropertyEntry, WallpaperType};
use kirie_formats::tex::{AnimationVersion, ContainerVersion, FreeImageFormat, Tex, TextureFlags};

use crate::detect::{self, FileKind};

pub fn run(path: &Path) -> Result<()> {
    if path.is_dir() {
        return info_item_dir(path);
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    match detect::detect(path, &bytes) {
        Some(FileKind::Project) => {
            let project = Project::from_path(path).with_context(|| format!("parsing {}", path.display()))?;
            println!("project.json: {}", path.display());
            print_project(&project);
            if let Some(dir) = path.parent() {
                print_scene_pkg_summary(dir, &project)?;
            }
            Ok(())
        }
        Some(FileKind::Pkg) => info_pkg(path, bytes),
        Some(FileKind::Tex) => info_tex(path, &bytes),
        None => bail!(
            "cannot determine the type of {} (expected a workshop item directory, \
             project.json, scene.pkg, or .tex)",
            path.display()
        ),
    }
}

fn info_item_dir(dir: &Path) -> Result<()> {
    let manifest = dir.join("project.json");
    if !manifest.is_file() {
        bail!(
            "{} does not contain a project.json (not a workshop item directory)",
            dir.display()
        );
    }
    let project = Project::from_path(&manifest).with_context(|| format!("parsing {}", manifest.display()))?;
    println!("workshop item: {}", dir.display());
    print_project(&project);
    print_scene_pkg_summary(dir, &project)
}

fn print_project(project: &Project) {
    println!("title: {}", project.title);
    match &project.declared_type {
        Some(declared) => println!(
            "type: {} (declared {:?})",
            type_name(project.resolved_type),
            declared.0
        ),
        None => println!("type: {} (no declared type)", type_name(project.resolved_type)),
    }
    println!("file: {}", project.file);
    if let Some(id) = &project.workshopid {
        println!("workshopid: {id}");
    }
    if let Some(category) = project.category() {
        println!("category: {category}");
    }
    if project.general.supportsaudioprocessing {
        println!("audio processing: yes");
    }
    let properties = &project.general.properties;
    if properties.is_empty() {
        println!("properties: 0");
    } else {
        let list: Vec<String> = properties
            .iter()
            .map(|(key, entry)| match entry {
                PropertyEntry::Property(p) => format!("{key}: {}", p.kind.type_tag()),
                PropertyEntry::Group(_) => format!("{key}: group"),
                PropertyEntry::Unrecognized(_) => format!("{key}: unrecognized"),
            })
            .collect();
        println!("properties: {} ({})", properties.len(), list.join(", "));
    }
}

fn print_scene_pkg_summary(dir: &Path, project: &Project) -> Result<()> {
    if project.resolved_type != WallpaperType::Scene {
        return Ok(());
    }
    let pkg_path = dir.join("scene.pkg");
    if !pkg_path.is_file() {
        println!("scene.pkg: not present");
        return Ok(());
    }
    let pkg = OwnedPkg::from_path(&pkg_path).with_context(|| format!("parsing {}", pkg_path.display()))?;
    println!(
        "scene.pkg: {} entries, {} bytes",
        pkg.entry_count(),
        pkg.as_bytes().len()
    );
    Ok(())
}

fn info_pkg(path: &Path, bytes: Vec<u8>) -> Result<()> {
    let size = bytes.len();
    let pkg = OwnedPkg::from_vec(bytes).with_context(|| format!("parsing {}", path.display()))?;
    println!("scene.pkg: {} ({size} bytes)", path.display());
    println!(
        "magic: {} (version {})",
        String::from_utf8_lossy(pkg.magic()),
        String::from_utf8_lossy(pkg.version())
    );
    println!("entries: {}", pkg.entry_count());
    let mut total: u64 = 0;
    for entry in pkg.entries() {
        total += u64::from(entry.len);
        println!("  {}  {} bytes", String::from_utf8_lossy(entry.name), entry.len);
    }
    println!("total payload: {total} bytes");
    Ok(())
}

fn info_tex(path: &Path, bytes: &[u8]) -> Result<()> {
    let tex = Tex::parse(bytes).with_context(|| format!("parsing {}", path.display()))?;
    println!("tex: {} ({} bytes)", path.display(), bytes.len());
    println!(
        "container: {} (effective {})",
        container_name(tex.container),
        container_name(tex.effective_container())
    );
    println!("format: {:?}", tex.format);
    println!("flags: 0x{:08x} ({})", tex.flags.0, flag_names(tex.flags));
    println!("fif: {}", fif_desc(tex.fif));
    println!("stored size: {}x{}", tex.texture_width, tex.texture_height);
    println!("image size: {}x{}", tex.width, tex.height);
    println!("images: {}", tex.images.len());
    for (index, image) in tex.images.iter().enumerate() {
        let dims = match (image.mipmaps.first(), image.mipmaps.last()) {
            (Some(first), Some(last)) if image.mipmaps.len() > 1 => format!(
                ", {}x{} -> {}x{}",
                first.width, first.height, last.width, last.height
            ),
            (Some(first), _) => format!(", {}x{}", first.width, first.height),
            _ => String::new(),
        };
        println!("  image {index}: {} mip level(s){dims}", image.mipmaps.len());
    }
    println!("video: {}", if tex.is_video() { "yes" } else { "no" });
    if let Some(anim) = &tex.animation {
        let total: f64 = anim.frames.iter().map(|f| f64::from(f.frametime)).sum();
        println!(
            "animation: {}, {} frames, gif {}x{}, {total:.3}s loop",
            animation_name(anim.version),
            anim.frames.len(),
            anim.gif_width,
            anim.gif_height
        );
    }
    Ok(())
}

fn type_name(kind: WallpaperType) -> &'static str {
    match kind {
        WallpaperType::Scene => "scene",
        WallpaperType::Web => "web",
        WallpaperType::Video => "video",
        WallpaperType::Image => "image",
        WallpaperType::Application => "application",
    }
}

fn container_name(version: ContainerVersion) -> &'static str {
    match version {
        ContainerVersion::Texb0001 => "TEXB0001",
        ContainerVersion::Texb0002 => "TEXB0002",
        ContainerVersion::Texb0003 => "TEXB0003",
        ContainerVersion::Texb0004 => "TEXB0004",
    }
}

fn animation_name(version: AnimationVersion) -> &'static str {
    match version {
        AnimationVersion::Texs0001 => "TEXS0001",
        AnimationVersion::Texs0002 => "TEXS0002",
        AnimationVersion::Texs0003 => "TEXS0003",
    }
}

fn flag_names(flags: TextureFlags) -> String {
    let mut names = Vec::new();
    if flags.no_interpolation() {
        names.push("NoInterpolation");
    }
    if flags.clamp_uvs() {
        names.push("ClampUVs");
    }
    if flags.is_gif() {
        names.push("IsGif");
    }
    if flags.clamp_uvs_border() {
        names.push("ClampUVsBorder");
    }
    if flags.video() {
        names.push("Video");
    }
    if flags.alpha_channel_priority() {
        names.push("AlphaChannelPriority");
    }
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join("|")
    }
}

fn fif_desc(fif: FreeImageFormat) -> String {
    if fif == FreeImageFormat::UNKNOWN {
        "-1 (raw)".to_owned()
    } else if fif == FreeImageFormat::PNG {
        "13 (PNG)".to_owned()
    } else if fif == FreeImageFormat::JPEG {
        "2 (JPEG)".to_owned()
    } else if fif == FreeImageFormat::MP4 {
        "35 (MP4)".to_owned()
    } else {
        format!("{}", fif.0)
    }
}
