use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use image::RgbaImage;
use kirie_formats::pkg::Pkg;
use kirie_formats::tex::{Frame, Tex};

use crate::detect::{self, FileKind};

pub fn run(path: &Path, out_dir: &Path, tex_to_png: bool) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    match detect::detect(path, &bytes) {
        Some(FileKind::Pkg) => extract_pkg(path, &bytes, out_dir, tex_to_png),
        Some(FileKind::Tex) => extract_tex_file(path, &bytes, out_dir),
        Some(FileKind::Project) => bail!(
            "{} looks like a project.json manifest; extract takes a scene.pkg or .tex",
            path.display()
        ),
        None => bail!(
            "cannot determine the type of {} (expected a scene.pkg or .tex)",
            path.display()
        ),
    }
}

fn extract_pkg(path: &Path, bytes: &[u8], out_dir: &Path, tex_to_png: bool) -> Result<()> {
    let pkg = Pkg::parse(bytes).with_context(|| format!("parsing {}", path.display()))?;
    std::fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let mut written = 0usize;
    for entry in pkg.entries() {
        let name = entry.name_str().ok_or_else(|| {
            anyhow!(
                "entry name {:?} is not valid UTF-8",
                String::from_utf8_lossy(entry.name)
            )
        })?;
        let rel = sanitize_entry_path(name)?;
        let payload = pkg.read(entry).with_context(|| format!("reading entry {name}"))?;
        let dest = out_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&dest, payload).with_context(|| format!("cannot write {}", dest.display()))?;
        println!("{}", dest.display());
        written += 1;

        if tex_to_png && name.to_ascii_lowercase().ends_with(".tex") {
            let tex = Tex::parse(payload).with_context(|| format!("parsing texture {name}"))?;
            if tex.is_video() {
                eprintln!("skipping {name}: video texture (docs/format-tex.md §7.3)");
                continue;
            }
            let stem = rel.with_extension("");
            for png in
                write_tex_pngs(&tex, out_dir, &stem).with_context(|| format!("decoding texture {name}"))?
            {
                println!("{}", png.display());
            }
        }
    }
    println!("extracted {written} entries to {}", out_dir.display());
    Ok(())
}

fn extract_tex_file(path: &Path, bytes: &[u8], out_dir: &Path) -> Result<()> {
    let tex = Tex::parse(bytes).with_context(|| format!("parsing {}", path.display()))?;
    if tex.is_video() {
        bail!(
            "{} is a video texture (docs/format-tex.md §7.3): it stores an MP4 \
             stream, not decodable pixels",
            path.display()
        );
    }
    std::fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let stem = path
        .file_stem()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("texture"));
    let pngs =
        write_tex_pngs(&tex, out_dir, &stem).with_context(|| format!("decoding {}", path.display()))?;
    for png in &pngs {
        println!("{}", png.display());
    }
    println!("wrote {} PNG(s) to {}", pngs.len(), out_dir.display());
    Ok(())
}

fn sanitize_entry_path(name: &str) -> Result<PathBuf> {
    ensure!(!name.is_empty(), "empty entry name");
    ensure!(!name.starts_with('/'), "absolute entry path {name:?}");
    let mut out = PathBuf::new();
    for component in name.split('/') {
        ensure!(!component.is_empty(), "empty path component in entry {name:?}");
        ensure!(
            component != "." && component != "..",
            "path traversal in entry {name:?}"
        );
        ensure!(
            !component.contains('\\') && !component.contains('\0'),
            "unsafe character in entry {name:?}"
        );
        out.push(component);
    }
    Ok(out)
}

fn write_tex_pngs(tex: &Tex<'_>, out_dir: &Path, rel_stem: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    if let Some(anim) = &tex.animation {
        let mut atlases: HashMap<u32, RgbaImage> = HashMap::new();
        for (index, frame) in anim.frames.iter().enumerate() {
            let atlas = match atlases.entry(frame.frame_number) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let image_index = usize::try_from(frame.frame_number)
                        .with_context(|| format!("frame {index} frameNumber overflow"))?;
                    entry.insert(decode_mip0(tex, image_index)?)
                }
            };
            let cropped = crop_frame(atlas, frame, index)?;
            written.push(save_png(
                cropped,
                out_dir,
                rel_stem,
                &format!(".frame{index:03}"),
            )?);
        }
    } else {
        for index in 0..tex.images.len() {
            let image = decode_mip0(tex, index)?;
            let suffix = if tex.images.len() == 1 {
                String::new()
            } else {
                format!(".image{index}")
            };
            written.push(save_png(image, out_dir, rel_stem, &suffix)?);
        }
    }
    Ok(written)
}

fn decode_mip0(tex: &Tex<'_>, image_index: usize) -> Result<RgbaImage> {
    let decoded = tex.decode_rgba8(image_index, 0)?;
    RgbaImage::from_raw(decoded.width, decoded.height, decoded.pixels)
        .ok_or_else(|| anyhow!("image {image_index}: decoded pixel buffer size mismatch"))
}

fn crop_frame(atlas: &RgbaImage, frame: &Frame, index: usize) -> Result<RgbaImage> {
    ensure!(
        frame.width2 == 0.0 && frame.height2 == 0.0,
        "frame {index} is stored rotated in the atlas (width2/height2 != 0), \
         which is UNVERIFIED (docs/format-tex.md §8.1)"
    );
    let x = texel(frame.x, "x", index)?;
    let y = texel(frame.y, "y", index)?;
    let width = texel(frame.width1, "width1", index)?;
    let height = texel(frame.height1, "height1", index)?;
    ensure!(
        width > 0 && height > 0,
        "frame {index} has an empty rect {width}x{height}"
    );
    let end_x = x
        .checked_add(width)
        .ok_or_else(|| anyhow!("frame {index} rect overflows"))?;
    let end_y = y
        .checked_add(height)
        .ok_or_else(|| anyhow!("frame {index} rect overflows"))?;
    ensure!(
        end_x <= atlas.width() && end_y <= atlas.height(),
        "frame {index} rect {width}x{height}@{x},{y} exceeds its {}x{} atlas page",
        atlas.width(),
        atlas.height()
    );
    Ok(image::imageops::crop_imm(atlas, x, y, width, height).to_image())
}

fn texel(value: f32, what: &str, index: usize) -> Result<u32> {
    ensure!(
        value.is_finite() && value >= 0.0 && value <= u32::MAX as f32,
        "frame {index} field {what} = {value} is not a valid texel coordinate"
    );
    Ok(value.round() as u32)
}

fn save_png(image: RgbaImage, out_dir: &Path, rel_stem: &Path, suffix: &str) -> Result<PathBuf> {
    let mut name = rel_stem
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "texture".to_owned());
    name.push_str(suffix);
    name.push_str(".png");
    let dest = out_dir
        .join(rel_stem.parent().unwrap_or_else(|| Path::new("")))
        .join(name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    image
        .save_with_format(&dest, image::ImageFormat::Png)
        .with_context(|| format!("cannot write {}", dest.display()))?;
    Ok(dest)
}
