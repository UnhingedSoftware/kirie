use std::fs;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use crate::bundle::{ArchivedBakedBundle, BUNDLE_MAGIC, BakedBundle, BundleContent};
use crate::error::BakeError;
use crate::key::BundleKey;

const BUNDLE_FILE: &str = "bundle.rkyv";
const CHECKSUM_FILE: &str = "bundle.b3";
const ATIME_FILE: &str = ".atime";

#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn open_default() -> Result<Self, BakeError> {
        let base = default_cache_base()?;
        Ok(Self::with_root(base))
    }

    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Cache { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn bundles_dir(&self) -> PathBuf {
        self.root.join("bundles")
    }

    #[must_use]
    pub fn bundle_dir(&self, source: &[u8]) -> PathBuf {
        self.bundles_dir().join(BundleKey::compute(source).to_hex())
    }

    #[must_use]
    pub fn pipeline_cache_dir(&self, adapter_id: &str, source: &[u8]) -> PathBuf {
        self.root
            .join("pipelines")
            .join(sanitize(adapter_id))
            .join(BundleKey::compute(source).to_hex())
    }

    pub fn bake(&self, source: &[u8], content: BundleContent) -> Result<PathBuf, BakeError> {
        let dir = self.bundle_dir(source);
        fs::create_dir_all(&dir).map_err(|e| BakeError::io(&dir, e))?;

        let bundle: BakedBundle = content.into_bundle(source);
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&bundle)
            .map_err(|e| BakeError::Serialize(e.to_string()))?;
        let checksum = blake3::hash(&bytes);

        let file = dir.join(BUNDLE_FILE);
        write_atomic(&file, &bytes)?;
        write_atomic(&dir.join(CHECKSUM_FILE), checksum.to_hex().as_bytes())?;
        touch(&dir.join(ATIME_FILE))?;
        Ok(file)
    }

    pub fn load(&self, source: &[u8]) -> Result<Option<LoadedBundle>, BakeError> {
        let dir = self.bundle_dir(source);
        let file = dir.join(BUNDLE_FILE);
        if !file.exists() {
            return Ok(None);
        }
        match LoadedBundle::open(&file) {
            Ok(b) => {
                let _ = touch(&dir.join(ATIME_FILE));
                Ok(Some(b))
            }
            Err(BakeError::Corrupt { .. } | BakeError::ChecksumMismatch { .. }) => {
                let _ = fs::remove_dir_all(&dir);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub fn remove(&self, source: &[u8]) -> Result<(), BakeError> {
        let dir = self.bundle_dir(source);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(BakeError::io(&dir, e)),
        }
    }
}

pub struct LoadedBundle {
    mmap: Mmap,
    path: PathBuf,
}

impl std::fmt::Debug for LoadedBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedBundle")
            .field("path", &self.path)
            .field("size_bytes", &self.mmap.len())
            .finish()
    }
}

impl LoadedBundle {
    pub fn open(file: &Path) -> Result<Self, BakeError> {
        let f = fs::File::open(file).map_err(|e| BakeError::io(file, e))?;
        // SAFETY: the bundle file is opened read-only and this process never
        let mmap = unsafe { Mmap::map(&f) }.map_err(|e| BakeError::io(file, e))?;

        let sidecar = file.with_file_name(CHECKSUM_FILE);
        if let Ok(expected_hex) = fs::read_to_string(&sidecar) {
            let expected = expected_hex.trim();
            let actual = blake3::hash(&mmap).to_hex();
            if !expected.is_empty() && expected != actual.as_str() {
                return Err(BakeError::ChecksumMismatch {
                    path: file.to_path_buf(),
                    expected: expected.to_string(),
                    actual: actual.to_string(),
                });
            }
        }

        let archived = rkyv::access::<ArchivedBakedBundle, rkyv::rancor::Error>(&mmap).map_err(|e| {
            BakeError::Corrupt {
                path: file.to_path_buf(),
                reason: e.to_string(),
            }
        })?;
        if archived.header.magic.to_native() != BUNDLE_MAGIC {
            return Err(BakeError::Corrupt {
                path: file.to_path_buf(),
                reason: format!(
                    "bad magic 0x{:08x} (expected 0x{BUNDLE_MAGIC:08x})",
                    archived.header.magic.to_native()
                ),
            });
        }

        Ok(LoadedBundle {
            mmap,
            path: file.to_path_buf(),
        })
    }

    #[must_use]
    pub fn archived(&self) -> &ArchivedBakedBundle {
        // SAFETY: the same bytes were validated with the checked `access` in
        unsafe { rkyv::access_unchecked::<ArchivedBakedBundle>(&self.mmap) }
    }

    pub fn scene_model(&self) -> Result<kirie_scene::SceneModel, BakeError> {
        serde_json::from_slice(self.scene_json_bytes()).map_err(|e| BakeError::Decode {
            field: "scene_json",
            reason: e.to_string(),
        })
    }

    #[must_use]
    pub fn scene_json_bytes(&self) -> &[u8] {
        &self.archived().scene_json
    }

    #[must_use]
    pub fn shader_count(&self) -> usize {
        self.archived().shaders.len()
    }

    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.archived().textures.len()
    }

    #[must_use]
    pub fn texture_data(&self, i: usize) -> Option<&[u8]> {
        self.archived().textures.get(i).map(|t| t.data.as_slice())
    }

    #[must_use]
    pub fn size_bytes(&self) -> usize {
        self.mmap.len()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn deserialize(&self) -> Result<BakedBundle, BakeError> {
        rkyv::deserialize::<BakedBundle, rkyv::rancor::Error>(self.archived()).map_err(|e| {
            BakeError::Corrupt {
                path: self.path.clone(),
                reason: e.to_string(),
            }
        })
    }
}

fn default_cache_base() -> Result<PathBuf, BakeError> {
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return Ok(PathBuf::from(x).join("kirie"));
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join(".cache").join("kirie"));
    }
    Err(BakeError::io(
        PathBuf::from("~/.cache/kirie"),
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "neither XDG_CACHE_HOME nor HOME is set",
        ),
    ))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), BakeError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tid: String = format!("{:?}", std::thread::current().id())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let tmp = dir.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("bundle"),
        std::process::id(),
        tid,
    ));
    fs::write(&tmp, bytes).map_err(|e| BakeError::io(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        BakeError::io(path, e)
    })
}

fn touch(path: &Path) -> Result<(), BakeError> {
    fs::write(path, []).map_err(|e| BakeError::io(path, e))
}
