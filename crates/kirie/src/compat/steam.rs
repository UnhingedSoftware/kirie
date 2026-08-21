//! Steam library discovery.
//!
//! The reference engine only ever looks under `$HOME`
//! (Steam/FileSystem/FileSystem.cpp:9-53), which finds Steam itself but not
//! the *libraries* it installs games into: Steam lets every library live on its
//! own disk, and records them in `steamapps/libraryfolders.vdf`. A Workshop
//! subscription (or Wallpaper Engine itself) sitting on a second drive is
//! therefore invisible to the reference — and was to kirie, which reported
//! `Cannot find workshop directory` on a machine that clearly had one.
//!
//! [`libraries`] returns every library root on this machine — the Steam
//! installs under `$HOME` first, in the reference's probe order, then whatever
//! `libraryfolders.vdf` points at. Callers append the subdirectory they want
//! (`steamapps/workshop/content/<app>`, `steamapps/common/<game>`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Steam installation roots relative to `$HOME` — the directory that holds
/// `steamapps`, one per install shape (native, `.steam` symlink tree, Flatpak,
/// Snap). Order matches the reference's probe order.
const STEAM_ROOTS: [&str; 4] = [
    ".local/share/Steam",
    ".steam/steam",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    "snap/steam/common/.local/share/Steam",
];

/// Every Steam library root on this machine, in probe order and without
/// duplicates.
///
/// `KIRIE_STEAM_LIBRARY` overrides the probe entirely (`:`-separated, like
/// `PATH`), for installs this cannot infer — a library on a removable disk, or
/// a Steam install in an unusual prefix.
#[must_use]
pub fn libraries() -> Vec<PathBuf> {
    libraries_with(
        std::env::var_os("KIRIE_STEAM_LIBRARY"),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// [`libraries`] with its two environment inputs passed in, so the override and
/// probe order are testable without mutating the process environment (this
/// crate forbids the `unsafe` that `set_var` now requires).
fn libraries_with(override_value: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(value) = override_value {
        return std::env::split_paths(&value).filter(|dir| dir.is_dir()).collect();
    }

    let Some(home) = home else {
        return Vec::new();
    };

    fn push(found: &mut Vec<PathBuf>, dir: PathBuf) {
        if dir.is_dir() && !found.contains(&dir) {
            found.push(dir);
        }
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for root in STEAM_ROOTS {
        push(&mut found, home.join(root));
    }

    // Each install indexes every library, including the ones on other disks.
    // Reading all of them (rather than stopping at the first) keeps a Flatpak
    // and a native Steam sharing one library working.
    for install in found.clone() {
        for library in libraryfolders(&install) {
            push(&mut found, library);
        }
    }

    found
}

/// Library paths recorded in an install's `libraryfolders.vdf`.
///
/// Steam has kept the file in two places and two shapes over the years, so
/// both are read and both entry forms are accepted.
fn libraryfolders(install: &Path) -> Vec<PathBuf> {
    ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"]
        .iter()
        .filter_map(|rel| std::fs::read_to_string(install.join(rel)).ok())
        .flat_map(|text| parse_libraryfolders(&text))
        .collect()
}

/// Pull library paths out of a `libraryfolders.vdf`.
///
/// Modern Steam writes a block per library with a `"path"` key; older versions
/// wrote `"1"  "/mnt/games"` directly. Rather than implement VDF, take the
/// second quoted string on any line whose first quoted string is `path` or a
/// number — the two forms that carry a library path.
fn parse_libraryfolders(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let mut quoted = line.split('"').skip(1).step_by(2);
            let key = quoted.next()?;
            if key != "path" && !key.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let value = quoted.next()?;
            if value.is_empty() {
                return None;
            }
            // VDF escapes backslashes; harmless on Linux paths, wrong to keep.
            Some(PathBuf::from(value.replace("\\\\", "\\")))
        })
        .collect()
}

/// Every existing `steamapps/<relative>` directory across all libraries.
#[must_use]
pub fn steamapps_dirs(relative: impl AsRef<Path>) -> Vec<PathBuf> {
    let relative = relative.as_ref();
    libraries()
        .into_iter()
        .map(|library| library.join("steamapps").join(relative))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// The paths [`steamapps_dirs`] probes, existing or not, for diagnostics.
#[must_use]
pub fn steamapps_candidates(relative: impl AsRef<Path>) -> Vec<PathBuf> {
    let relative = relative.as_ref();
    libraries()
        .into_iter()
        .map(|library| library.join("steamapps").join(relative))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_libraryfolders() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/user/.local/share/Steam"
		"label"		""
		"contentid"		"1234567890"
	}
	"1"
	{
		"path"		"/mnt/games/SteamLibrary"
		"label"		""
	}
}
"#;
        assert_eq!(
            parse_libraryfolders(vdf),
            vec![
                PathBuf::from("/home/user/.local/share/Steam"),
                PathBuf::from("/mnt/games/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn parses_legacy_numbered_entries() {
        let vdf = r#"
"LibraryFolders"
{
	"TimeNextStatsReport"		"1234567890"
	"ContentStatsID"		"-1234567890123456789"
	"1"		"/mnt/games/SteamLibrary"
	"2"		"/run/media/user/disk/SteamLibrary"
}
"#;
        // The stats keys are not numeric, so only the real entries survive.
        assert_eq!(
            parse_libraryfolders(vdf),
            vec![
                PathBuf::from("/mnt/games/SteamLibrary"),
                PathBuf::from("/run/media/user/disk/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn ignores_empty_and_unrelated_keys() {
        let vdf = "\"label\"\t\t\"\"\n\"path\"\t\t\"\"\n\"apps\"\n{\n}\n";
        assert!(parse_libraryfolders(vdf).is_empty());
    }

    #[test]
    fn override_wins_and_keeps_only_existing_dirs() {
        let tmp = std::env::temp_dir().join("kirie-steam-override-test");
        std::fs::create_dir_all(&tmp).expect("temp dir");

        let value =
            std::env::join_paths([tmp.as_path(), Path::new("/definitely/not/here")]).expect("join paths");
        let found = libraries_with(Some(value), Some(PathBuf::from("/nonexistent-home")));

        assert_eq!(found, vec![tmp.clone()]);
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn finds_the_library_a_libraryfolders_points_at() {
        // A Steam install under $HOME whose index points at a library on
        // another disk — the case that used to be invisible.
        let root = std::env::temp_dir().join("kirie-steam-discovery-test");
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let install = home.join(".local/share/Steam");
        let other_disk = root.join("mnt/games/SteamLibrary");
        std::fs::create_dir_all(install.join("steamapps")).expect("install");
        std::fs::create_dir_all(other_disk.join("steamapps")).expect("library");
        std::fs::write(
            install.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                other_disk.display()
            ),
        )
        .expect("vdf");

        let found = libraries_with(None, Some(home));

        assert!(found.contains(&install), "install itself: {found:?}");
        assert!(found.contains(&other_disk), "second disk: {found:?}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
