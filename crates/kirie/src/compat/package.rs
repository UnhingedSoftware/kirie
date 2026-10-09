//! Playing a `.kpk` package.
//!
//! The players read wallpapers from folders, so a package is unpacked once,
//! every entry checked against its hash, into a per-user cache named after
//! the package's fingerprint, and played from there. A package seen before
//! starts straight from its unpacked copy. Only the few most recently
//! unpacked packages are kept.

use std::path::{Path, PathBuf};

use kirie_pack::{Kind, Package};

use crate::compat::resolve::{ClassifyError, Wallpaper};

/// How many unpacked packages the cache keeps.
const KEEP: usize = 4;

pub fn classify(path: &Path) -> Result<Wallpaper, ClassifyError> {
    let refuse = |reason: String| ClassifyError::Package {
        path: path.to_path_buf(),
        reason,
    };
    let mut package = Package::open(path).map_err(|e| refuse(e.to_string()))?;
    let root =
        cache_root().ok_or_else(|| refuse("there is no cache directory to unpack it into".to_owned()))?;
    let dir = unpacked(&mut package, &root).map_err(refuse)?;
    let entry = package.manifest().entry.clone();
    match package.manifest().kind {
        Kind::WallpaperEngine => crate::compat::resolve::classify_dir(&dir),
        Kind::Video => Ok(Wallpaper::Video {
            media: dir.join(&entry),
        }),
        Kind::Image => Ok(Wallpaper::Image {
            file: dir.join(&entry),
        }),
        Kind::Web => Ok(Wallpaper::Web { dir, file: entry }),
        Kind::Scene => Ok(Wallpaper::Unsupported { kind: "kirie scene" }),
    }
}

/// The folder `package` is unpacked in under `root`, unpacking it first if
/// it is not there yet.
fn unpacked<R: std::io::Read + std::io::Seek>(
    package: &mut Package<R>,
    root: &Path,
) -> Result<PathBuf, String> {
    let fingerprint = package.fingerprint();
    let dir = root.join(&fingerprint);
    // A folder only ever gets this name by a rename of a complete unpack.
    if dir.is_dir() {
        return Ok(dir);
    }
    std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let partial = root.join(format!(".{fingerprint}.{}.partial", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    if let Err(err) = package.unpack_to(&partial) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!("cannot unpack it: {err}"));
    }
    if let Err(err) = std::fs::rename(&partial, &dir) {
        let _ = std::fs::remove_dir_all(&partial);
        // Another kirie unpacking the same package got there first.
        if !dir.is_dir() {
            return Err(format!("cannot move it into {}: {err}", dir.display()));
        }
    }
    forget_old(root, &dir);
    Ok(dir)
}

/// Remove all but the `KEEP` most recently unpacked packages, never `keep`.
fn forget_old(root: &Path, keep: &Path) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    let mut unpacked: Vec<(std::time::SystemTime, PathBuf)> = read
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, path)| path != keep)
        .collect();
    unpacked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, old) in unpacked.into_iter().skip(KEEP.saturating_sub(1)) {
        if let Err(err) = std::fs::remove_dir_all(&old) {
            tracing::debug!(dir = %old.display(), %err, "cannot remove an old unpacked package");
        }
    }
}

fn cache_root() -> Option<PathBuf> {
    Some(cache_home()?.join("kirie").join("packages"))
}

/// The per-user cache directory, under whatever name this platform gives it.
/// Windows sets neither `XDG_CACHE_HOME` nor `HOME`.
#[cfg(windows)]
fn cache_home() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn cache_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kirie_pack::{Builder, Compression, Manifest, Provenance};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kirie-package-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn video(id: &str) -> Package<Cursor<Vec<u8>>> {
        let manifest = Manifest {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            author: String::new(),
            kind: Kind::Video,
            entry: "loop.mp4".into(),
            preview: None,
            tags: vec![],
            mature: false,
            properties: vec![],
            min_kirie: None,
            provenance: Provenance::Original,
        };
        let mut b = Builder::new(manifest);
        b.add("loop.mp4", id.as_bytes().to_vec(), Compression::None)
            .unwrap();
        let mut out = Cursor::new(Vec::new());
        b.write(&mut out).unwrap();
        Package::from_reader(Cursor::new(out.into_inner())).unwrap()
    }

    #[test]
    fn a_package_is_unpacked_once_and_found_again() {
        let root = scratch("once");
        let mut p = video("rain");
        let dir = unpacked(&mut p, &root).unwrap();
        assert_eq!(std::fs::read(dir.join("loop.mp4")).unwrap(), b"rain");

        std::fs::write(dir.join("marker"), b"").unwrap();
        let again = unpacked(&mut video("rain"), &root).unwrap();
        assert_eq!(again, dir);
        assert!(again.join("marker").is_file(), "it was unpacked a second time");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_the_latest_few_stay_unpacked() {
        let root = scratch("latest");
        let mut dirs = Vec::new();
        for n in 0..KEEP + 2 {
            dirs.push(unpacked(&mut video(&format!("w{n}")), &root).unwrap());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let left = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(left, KEEP);
        assert!(dirs.last().unwrap().is_dir(), "the newest one was removed");
        assert!(!dirs[0].is_dir(), "the oldest one was kept");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
