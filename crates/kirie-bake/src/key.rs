use std::fmt;

pub const BAKE_FORMAT_VERSION: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BundleKey([u8; 32]);

impl BundleKey {
    #[must_use]
    pub fn compute(source: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(source);
        h.update(&BAKE_FORMAT_VERSION.to_le_bytes());
        h.update(&kirie_shader::TRANSLATOR_VERSION.to_le_bytes());
        h.update(&assets_fingerprint());
        BundleKey(*h.finalize().as_bytes())
    }

    #[must_use]
    pub fn bytes(&self) -> [u8; 32] {
        self.0
    }

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
