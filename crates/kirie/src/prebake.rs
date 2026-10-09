//! Pictures resized ahead of time to the screens they are shown on.
//!
//! The image renderer uploads a picture whole, with no mipmaps, and the GPU
//! shrinks it with a linear filter every frame. A 6000-pixel photo on a
//! 1920-pixel screen then costs 96 MB of video memory and shows the jagged
//! edges of sampling one texel in nine. A prebaked copy is the picture
//! resized once, with a Lanczos filter, to just cover the screen, and kept as
//! a JPEG (or a PNG when it is see-through) in kirie's cache, one per screen
//! size.
//!
//! The copy keeps the picture's aspect ratio, so every scaling mode frames it
//! exactly as it framed the original. Pictures already no larger than the
//! screen, animated GIFs and Wallpaper Engine textures are drawn as they are.
//!
//! `kirie prebake` bakes in advance for the screens kirie last ran on (or the
//! sizes given), so the first time a picture goes up is as quick as the rest.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Bumped when a change makes earlier bakes wrong, so they are made again.
const FORMAT: u32 = 1;
/// JPEG quality of a bake: indistinguishable from the original at screen size.
const QUALITY: u8 = 92;
/// The most the bake cache keeps before the least recently used go.
const CACHE_LIMIT: u64 = 512 * 1024 * 1024;

/// Pictures a bake can be made from. GIFs may move, and `.tex` is drawn by
/// its own decoder, so both are left alone.
const BAKEABLE: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "webp"];

/// The picture to draw on a screen of `screen` pixels: a bake when one helps,
/// otherwise `file` itself. Never fails; a bake that cannot be made is
/// reported and the original is drawn.
#[must_use]
pub fn for_screen(file: &Path, screen: (u32, u32)) -> PathBuf {
    if std::env::var_os("KIRIE_NO_PICTURE_BAKE").is_some() {
        return file.to_owned();
    }
    match bake(file, screen) {
        Ok(Some(baked)) => baked.path,
        Ok(None) => file.to_owned(),
        Err(err) => {
            tracing::warn!(file = %file.display(), "could not prebake the picture, drawing the original: {err:#}");
            file.to_owned()
        }
    }
}

/// What `bake` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baked {
    pub path: PathBuf,
    pub size: (u32, u32),
    /// Already in the cache from an earlier run.
    pub reused: bool,
}

/// The cached bake of `file` for a screen of `screen` pixels, made now if
/// there is none. `None` when the picture is drawn as it is.
///
/// # Errors
///
/// When the picture cannot be read or decoded, or the bake cannot be written.
pub fn bake(file: &Path, screen: (u32, u32)) -> Result<Option<Baked>> {
    let Some(dir) = cache_dir() else {
        return Ok(None);
    };
    bake_into(file, screen, &dir)
}

fn bake_into(file: &Path, screen: (u32, u32), dir: &Path) -> Result<Option<Baked>> {
    let ext = file
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !BAKEABLE.contains(&ext.as_str()) || screen.0 == 0 || screen.1 == 0 {
        return Ok(None);
    }

    // The header alone says whether there is anything to gain, so a picture
    // that is already small is never decoded twice.
    let reader = image::ImageReader::open(file)
        .with_context(|| format!("opening {}", file.display()))?
        .with_guessed_format()?;
    let (width, height) = reader.into_dimensions()?;
    let target = cover_size((width, height), screen);
    if target == (width, height) {
        return Ok(None);
    }

    let key = key(file, screen)?;
    for ext in ["jpg", "png"] {
        let path = dir.join(format!("{key}.{ext}"));
        if path.is_file() {
            touch(&path);
            return Ok(Some(Baked {
                path,
                size: target,
                reused: true,
            }));
        }
    }

    let mut decoder = image::ImageReader::open(file)?
        .with_guessed_format()?
        .into_decoder()?;
    // Photos from a phone are stored sideways and say so in EXIF.
    let orientation = image::ImageDecoder::orientation(&mut decoder).ok();
    let mut picture =
        image::DynamicImage::from_decoder(decoder).with_context(|| format!("decoding {}", file.display()))?;
    if let Some(orientation) = orientation {
        picture.apply_orientation(orientation);
    }
    let target = cover_size((picture.width(), picture.height()), screen);
    let resized = picture.resize_exact(target.0, target.1, image::imageops::FilterType::Lanczos3);

    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let see_through =
        resized.color().has_alpha() && resized.to_rgba8().pixels().any(|pixel| pixel.0[3] < 255);
    let ext = if see_through { "png" } else { "jpg" };
    let path = dir.join(format!("{key}.{ext}"));
    let staged = dir.join(format!(".{key}.{}.{ext}", std::process::id()));
    let written = write(&resized, see_through, &staged)
        .and_then(|()| std::fs::rename(&staged, &path).context("moving the bake into place"));
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written?;
    prune(dir, CACHE_LIMIT);
    tracing::info!(
        file = %file.display(),
        from = ?(width, height),
        to = ?target,
        "prebaked the picture for this screen"
    );
    Ok(Some(Baked {
        path,
        size: target,
        reused: false,
    }))
}

fn write(picture: &image::DynamicImage, see_through: bool, to: &Path) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .with_context(|| format!("creating {}", to.display()))?;
    let mut out = std::io::BufWriter::new(file);
    if see_through {
        picture.to_rgba8().write_to(&mut out, image::ImageFormat::Png)?;
    } else {
        let rgb = picture.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, QUALITY).encode_image(&rgb)?;
    }
    std::io::Write::flush(&mut out)?;
    Ok(())
}

/// The smallest size, at the picture's own aspect ratio, that covers
/// `screen`; the picture's own size when that is no bigger.
#[must_use]
pub fn cover_size(picture: (u32, u32), screen: (u32, u32)) -> (u32, u32) {
    let (pw, ph) = (f64::from(picture.0.max(1)), f64::from(picture.1.max(1)));
    let scale = (f64::from(screen.0) / pw).max(f64::from(screen.1) / ph);
    if scale >= 1.0 {
        return picture;
    }
    // Less a hair, so 6000 x 0.32 that comes out as 1920.0000000002 is 1920.
    let size = |side: f64| ((side * scale - 1e-6).ceil() as u32).max(1);
    (size(pw).min(picture.0), size(ph).min(picture.1))
}

/// Names a bake by the picture (where it is, its size and when it last
/// changed), the screen size and the bake format, so an edited picture is
/// baked again and two screens of one size share a bake.
fn key(file: &Path, screen: (u32, u32)) -> Result<String> {
    let meta = std::fs::metadata(file).with_context(|| format!("reading {}", file.display()))?;
    let changed = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    let source = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_owned());
    let mut hasher = blake3::Hasher::new();
    hasher.update(&FORMAT.to_le_bytes());
    hasher.update(source.to_string_lossy().as_bytes());
    hasher.update(&[0]);
    hasher.update(&meta.len().to_le_bytes());
    hasher.update(&changed.to_le_bytes());
    hasher.update(&screen.0.to_le_bytes());
    hasher.update(&screen.1.to_le_bytes());
    let hex = hasher.finalize().to_hex();
    Ok(format!("{}-{}x{}", &hex[..24], screen.0, screen.1))
}

fn cache_dir() -> Option<PathBuf> {
    kirie_platform::cache_home().map(|home| home.join("kirie").join("pictures"))
}

/// Marks a bake as just used, so pruning keeps it.
fn touch(path: &Path) {
    if let Ok(file) = std::fs::File::options().append(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Removes the least recently used bakes until the folder holds at most
/// `limit` bytes.
fn prune(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut bakes: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    meta.len(),
                    entry.path(),
                )
            })
        })
        .collect();
    let mut total: u64 = bakes.iter().map(|(_, len, _)| len).sum();
    bakes.sort();
    for (_, len, path) in bakes {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Screens kirie has drawn on, by name, with their size in pixels, so
/// `kirie prebake` knows what to bake for without a window of its own.
fn screens_file() -> Option<PathBuf> {
    kirie_platform::cache_home().map(|home| home.join("kirie").join("screens.json"))
}

/// Records the size of a screen kirie is drawing on.
pub fn remember_screen(name: &str, size: (u32, u32)) {
    let Some(path) = screens_file() else {
        return;
    };
    let mut known = read_screens(&path);
    if known
        .get(name)
        .and_then(serde_json::Value::as_array)
        .is_some_and(|seen| {
            seen.first().and_then(serde_json::Value::as_u64) == Some(u64::from(size.0))
                && seen.get(1).and_then(serde_json::Value::as_u64) == Some(u64::from(size.1))
        })
    {
        return;
    }
    known.insert(name.to_owned(), serde_json::json!([size.0, size.1]));
    let Some(parent) = path.parent() else {
        return;
    };
    let staged = parent.join(format!(".screens.{}.json", std::process::id()));
    let written = std::fs::create_dir_all(parent)
        .and_then(|()| std::fs::write(&staged, serde_json::Value::Object(known).to_string()))
        .and_then(|()| std::fs::rename(&staged, &path));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&staged);
        tracing::debug!("could not record the screen size: {err}");
    }
}

fn read_screens(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// The distinct screen sizes kirie has drawn on.
#[must_use]
pub fn remembered_sizes() -> Vec<(u32, u32)> {
    let Some(path) = screens_file() else {
        return Vec::new();
    };
    let mut sizes: Vec<(u32, u32)> = read_screens(&path)
        .values()
        .filter_map(|size| {
            let size = size.as_array()?;
            let width = u32::try_from(size.first()?.as_u64()?).ok()?;
            let height = u32::try_from(size.get(1)?.as_u64()?).ok()?;
            (width > 0 && height > 0).then_some((width, height))
        })
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

/// Parses `2560x1440`.
///
/// # Errors
///
/// When it is not two positive whole numbers joined by `x`.
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (width, height) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{text}: expected WIDTHxHEIGHT, such as 2560x1440"))?;
    let side = |side: &str| side.trim().parse::<u32>().ok().filter(|side| *side > 0);
    match (side(width), side(height)) {
        (Some(width), Some(height)) => Ok((width, height)),
        _ => Err(format!("{text}: expected WIDTHxHEIGHT, such as 2560x1440")),
    }
}

/// `kirie prebake`: bakes each picture named, or found in a folder named,
/// for each size given, or else for every screen kirie has drawn on.
///
/// # Errors
///
/// When no size is given and kirie has not drawn on any screen yet.
pub fn run(paths: &[PathBuf], sizes: &[(u32, u32)]) -> Result<bool> {
    let sizes = if sizes.is_empty() {
        remembered_sizes()
    } else {
        sizes.to_vec()
    };
    if sizes.is_empty() {
        anyhow::bail!(
            "no screen sizes known yet: run kirie on your screens once, or pass --size WIDTHxHEIGHT"
        );
    }
    let mut all_ok = true;
    for picture in paths.iter().flat_map(|path| pictures_in(path)) {
        for &size in &sizes {
            match bake(&picture, size) {
                Ok(Some(baked)) => println!(
                    "{}  {}x{}  {}{}",
                    picture.display(),
                    baked.size.0,
                    baked.size.1,
                    baked.path.display(),
                    if baked.reused { "  (already baked)" } else { "" }
                ),
                Ok(None) => println!("{}  {}x{}  drawn as it is", picture.display(), size.0, size.1),
                Err(err) => {
                    all_ok = false;
                    eprintln!("{}: {err:#}", picture.display());
                }
            }
        }
    }
    Ok(all_ok)
}

/// The pictures `path` names: itself, the picture of a wallpaper folder, or
/// the pictures directly inside a plain folder.
fn pictures_in(path: &Path) -> Vec<PathBuf> {
    match crate::compat::resolve::classify(&path.to_string_lossy()) {
        Ok(crate::compat::resolve::Wallpaper::Image { file }) => vec![file],
        Ok(_) => Vec::new(),
        Err(_) if path.is_dir() => {
            let mut found: Vec<PathBuf> = std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|file| {
                    file.is_file()
                        && file.extension().is_some_and(|ext| {
                            BAKEABLE.contains(&ext.to_string_lossy().to_ascii_lowercase().as_str())
                        })
                })
                .collect();
            found.sort();
            found
        }
        Err(_) => {
            eprintln!("{}: not found", path.display());
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kirie-prebake-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn picture(at: &Path, size: (u32, u32), alpha: u8) {
        let mut canvas = image::RgbaImage::new(size.0, size.1);
        for (x, y, pixel) in canvas.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 251) as u8, (y % 241) as u8, 90, alpha]);
        }
        canvas.save(at).expect("scratch picture");
    }

    #[test]
    fn the_bake_just_covers_the_screen_at_the_pictures_shape() {
        assert_eq!(cover_size((6000, 4000), (1920, 1080)), (1920, 1280));
        assert_eq!(cover_size((4000, 6000), (1920, 1080)), (1920, 2880));
        assert_eq!(cover_size((1000, 500), (1920, 1080)), (1000, 500));
        assert_eq!(cover_size((3840, 2160), (3840, 2160)), (3840, 2160));
        assert_eq!(cover_size((7680, 4320), (2560, 1440)), (2560, 1440));
    }

    #[test]
    fn a_big_picture_is_baked_once_per_screen_size() {
        let scratch = Scratch::new("big");
        let source = scratch.0.join("photo.png");
        picture(&source, (1600, 1000), 255);
        let cache = scratch.0.join("cache");

        let first = bake_into(&source, (800, 450), &cache)
            .expect("bake")
            .expect("a bake");
        assert!(!first.reused);
        assert_eq!(first.size, (800, 500));
        assert_eq!(first.path.extension().and_then(|e| e.to_str()), Some("jpg"));
        assert_eq!(image::image_dimensions(&first.path).ok(), Some((800, 500)));

        let again = bake_into(&source, (800, 450), &cache)
            .expect("bake")
            .expect("a bake");
        assert!(again.reused);
        assert_eq!(again.path, first.path);

        let other = bake_into(&source, (640, 400), &cache)
            .expect("bake")
            .expect("a bake");
        assert_ne!(other.path, first.path);
        assert_eq!(other.size, (640, 400));
    }

    #[test]
    fn a_see_through_picture_stays_see_through() {
        let scratch = Scratch::new("alpha");
        let source = scratch.0.join("logo.png");
        picture(&source, (1200, 1200), 128);
        let baked = bake_into(&source, (600, 600), &scratch.0.join("cache"))
            .expect("bake")
            .expect("a bake");
        assert_eq!(baked.path.extension().and_then(|e| e.to_str()), Some("png"));
    }

    #[test]
    fn what_gains_nothing_is_drawn_as_it_is() {
        let scratch = Scratch::new("small");
        let cache = scratch.0.join("cache");
        let small = scratch.0.join("small.png");
        picture(&small, (640, 360), 255);
        assert_eq!(bake_into(&small, (1920, 1080), &cache).ok(), Some(None));

        let gif = scratch.0.join("moves.gif");
        let _ = std::fs::write(&gif, b"GIF89a");
        assert_eq!(bake_into(&gif, (100, 100), &cache).ok(), Some(None));
        assert!(!cache.exists());
    }

    #[test]
    fn an_edited_picture_is_baked_again() {
        let scratch = Scratch::new("edited");
        let source = scratch.0.join("photo.png");
        picture(&source, (1600, 1000), 255);
        let before = key(&source, (800, 450)).expect("key");
        picture(&source, (1600, 1001), 255);
        assert_ne!(key(&source, (800, 450)).expect("key"), before);
    }

    #[test]
    fn pruning_drops_the_least_recently_used_first() {
        let scratch = Scratch::new("prune");
        let old = scratch.0.join("old.jpg");
        let new = scratch.0.join("new.jpg");
        std::fs::write(&old, [0_u8; 600]).expect("write");
        std::fs::write(&new, [0_u8; 600]).expect("write");
        let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .append(true)
            .open(&old)
            .and_then(|file| file.set_modified(long_ago))
            .expect("age");
        prune(&scratch.0, 1000);
        assert!(!old.exists());
        assert!(new.exists());
    }

    #[test]
    fn sizes_read_the_way_people_write_them() {
        assert_eq!(parse_size("2560x1440"), Ok((2560, 1440)));
        assert_eq!(parse_size("1920X1080"), Ok((1920, 1080)));
        assert!(parse_size("1920").is_err());
        assert!(parse_size("0x1080").is_err());
        assert!(parse_size("wide").is_err());
    }
}
