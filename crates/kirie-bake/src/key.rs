//! Bundle cache keys (SPEC.md §V8).
//!
//! > V8: bundle key = blake3(source) ⊕ bake-format ver ⊕ shader-translator ver.
//! > Key mismatch → rebake. ⊥ migration code.
//!
//! We realize the `⊕`-of-versions as domain-separated hasher updates: the key is
//! `blake3(source ‖ BAKE_FORMAT_VERSION_le ‖ TRANSLATOR_VERSION_le)`. Any change
//! to the source bytes, the bundle layout, or the shader translator yields a
//! fresh digest → a different cache directory → a guaranteed miss → a rebake,
//! with no on-disk migration path (§V8). This is stronger than an integer XOR
//! (which could alias) while serving the same invariant.

use std::fmt;

/// On-disk bundle layout version. Bump whenever the [`crate::BakedBundle`] shape
/// or its encoding changes so every prior bundle keys to a different directory
/// and is transparently re-baked (SPEC.md §V8 — no migration).
pub const BAKE_FORMAT_VERSION: u32 = 2;

/// The 256-bit content-addressed key for a bundle (SPEC.md §V8). Its lowercase
/// hex form names the cache subdirectory.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BundleKey([u8; 32]);

impl BundleKey {
    /// Compute the key for `source` — the raw `scene.pkg` / `project.json` bytes
    /// that define the wallpaper — mixed with [`BAKE_FORMAT_VERSION`],
    /// [`kirie_shader::TRANSLATOR_VERSION`] and the shared-assets fingerprint
    /// (SPEC.md §V8).
    ///
    /// The fingerprint is load-bearing: a bundle bakes shaders assembled from
    /// Wallpaper Engine's shared asset headers, and Steam updates those
    /// underneath us. Without it, an asset update leaves every bundle
    /// stale-but-hitting — rendering with the previous asset generation while
    /// freshly-built wallpapers use the new one, a divergence that shows up as
    /// wallpapers breaking or healing at random as caches turn over.
    #[must_use]
    pub fn compute(source: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(source);
        // Domain-separate the versions so bumping either changes the key.
        h.update(&BAKE_FORMAT_VERSION.to_le_bytes());
        h.update(&kirie_shader::TRANSLATOR_VERSION.to_le_bytes());
        h.update(&assets_fingerprint());
        BundleKey(*h.finalize().as_bytes())
    }

    /// The raw 32-byte digest.
    #[must_use]
    pub fn bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Lowercase hex, used as the cache directory name.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// A cheap fingerprint of the Wallpaper Engine shared shader assets: file
/// name, size and mtime of everything under `assets/shaders`, hashed. Metadata
/// only — hashing hundreds of megabytes of asset content per launch is not
/// acceptable, and Steam always rewrites mtimes when it patches. Cached for
/// the process lifetime; an empty fingerprint (no assets found) is itself a
/// stable value.
fn assets_fingerprint() -> [u8; 32] {
    use std::sync::OnceLock;
    static FP: OnceLock<[u8; 32]> = OnceLock::new();
    *FP.get_or_init(|| {
        let mut entries: Vec<String> = Vec::new();
        if let Some(dir) = crate::we_assets_shaders_dir() {
            collect_meta(&dir, &dir, &mut entries);
        }
        entries.sort();
        let mut h = blake3::Hasher::new();
        for e in &entries {
            h.update(e.as_bytes());
            h.update(&[0]);
        }
        *h.finalize().as_bytes()
    })
}

/// Recursively record `relpath:len:mtime` for every file under `dir`.
fn collect_meta(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_meta(root, &path, out);
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        out.push(format!("{rel}:{}:{mtime}", meta.len()));
    }
}

impl fmt::Debug for BundleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BundleKey({})", self.to_hex())
    }
}

impl fmt::Display for BundleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_source_same_key() {
        assert_eq!(BundleKey::compute(b"abc"), BundleKey::compute(b"abc"));
    }

    #[test]
    fn source_change_changes_key() {
        assert_ne!(BundleKey::compute(b"abc"), BundleKey::compute(b"abd"));
    }

    #[test]
    fn hex_is_64_chars() {
        assert_eq!(BundleKey::compute(b"x").to_hex().len(), 64);
    }
}
