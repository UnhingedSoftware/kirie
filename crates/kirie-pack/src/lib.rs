//! The `.kpk` wallpaper package.
//!
//! One wallpaper is one file. It starts with a fixed 64-byte header, then the
//! entries (the wallpaper's own files), each starting on a 4 KiB boundary so a
//! player can hand a video stream or texture to its decoder straight from the
//! file, then the manifest and the index, both JSON, at the end. Putting the
//! directory last lets a writer stream entries out without knowing the
//! directory's size first; the header is patched with its position at the end.
//!
//! ```text
//! 0      header (64 bytes, zero-padded to 4096)
//! 4096   entry 0, zero-padded to the next 4096
//! ...    entry n
//!        manifest JSON
//!        index JSON
//! ```
//!
//! Header, all little-endian:
//!
//! | offset | size | field |
//! | --- | --- | --- |
//! | 0 | 8 | magic `KIRIEPKG` |
//! | 8 | 2 | format version, 1 |
//! | 10 | 2 | flags, 0 in v1 (bits reserved for encryption and signing) |
//! | 12 | 4 | reserved, 0 |
//! | 16 | 8 | manifest offset |
//! | 24 | 8 | manifest length |
//! | 32 | 8 | index offset |
//! | 40 | 8 | index length |
//! | 48 | 16 | first 16 bytes of blake3(manifest bytes, then index bytes) |
//!
//! Every entry carries the blake3 hash of the bytes as stored, so a damaged or
//! altered download is refused before anything decodes it.

mod error;
mod manifest;
mod read;
mod write;

pub use error::PackError;
pub use manifest::{Choice, Kind, Manifest, Property, PropertyValue, Provenance};
pub use read::{Entry, Package, Span};
pub use write::{Builder, Compression, Summary, pack_dir};

/// The extension a package file carries.
pub const EXTENSION: &str = "kpk";

/// The name of the manifest file in a folder `pack_dir` turns into a package.
pub const MANIFEST_FILE: &str = "kirie.json";

pub(crate) const MAGIC: [u8; 8] = *b"KIRIEPKG";
pub(crate) const FORMAT_VERSION: u16 = 1;
pub(crate) const HEADER_LEN: usize = 64;
pub(crate) const ALIGN: u64 = 4096;

/// A manifest larger than this is refused rather than parsed.
pub(crate) const MAX_MANIFEST_LEN: u64 = 1 << 20;
/// An index larger than this is refused rather than parsed.
pub(crate) const MAX_INDEX_LEN: u64 = 64 << 20;
/// A compressed entry may not claim to expand beyond this, so a hostile file
/// cannot make the reader allocate without limit.
pub(crate) const MAX_UNPACKED_ENTRY_LEN: u64 = 1 << 30;

pub(crate) fn align_up(n: u64) -> u64 {
    n.div_ceil(ALIGN) * ALIGN
}

pub(crate) fn directory_hash(manifest: &[u8], index: &[u8]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(manifest);
    hasher.update(index);
    let mut out = [0u8; 16];
    out.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    out
}

/// Check that `path` is a relative, `/`-separated path that cannot name
/// anything outside the package when a tool unpacks it on any platform.
pub(crate) fn check_entry_path(path: &str) -> Result<(), PackError> {
    let bad = |why: &'static str| PackError::BadPath {
        path: path.to_owned(),
        why,
    };
    if path.is_empty() {
        return Err(bad("it is empty"));
    }
    if path.len() > 1024 {
        return Err(bad("it is longer than 1024 bytes"));
    }
    if path.starts_with('/') {
        return Err(bad("it is absolute"));
    }
    if path.contains('\\') || path.contains(':') {
        return Err(bad("it contains \\ or :"));
    }
    if path.chars().any(char::is_control) {
        return Err(bad("it contains a control character"));
    }
    for part in path.split('/') {
        match part {
            "" => return Err(bad("it has an empty component")),
            "." | ".." => return Err(bad("it has a . or .. component")),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_could_escape_are_refused() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/./b",
            "c:x",
            "a\\b",
            "a\nb",
            "a/",
        ] {
            assert!(check_entry_path(bad).is_err(), "{bad:?} should be refused");
        }
        for good in ["scene.bin", "shaders/water.spv", "web/index.html", "a b/c-d_e.f"] {
            check_entry_path(good).unwrap();
        }
    }

    #[test]
    fn align_up_rounds_to_4k() {
        assert_eq!(align_up(0), 0);
        assert_eq!(align_up(1), 4096);
        assert_eq!(align_up(4096), 4096);
        assert_eq!(align_up(4097), 8192);
    }
}
