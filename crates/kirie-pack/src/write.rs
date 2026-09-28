use std::collections::HashSet;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::read::{Entry, Index};
use crate::{
    ALIGN, FORMAT_VERSION, HEADER_LEN, MAGIC, MANIFEST_FILE, Manifest, PackError, align_up, check_entry_path,
    directory_hash,
};

/// How an entry should be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// As is. Right for data that is already compressed (video, PNG, KTX2)
    /// and for anything the player wants to read in place.
    None,
    /// LZ4, kept only when it saves at least an eighth of the size.
    Lz4,
}

impl Compression {
    /// A sensible default for a file with this name: leave formats that are
    /// already compressed alone, compress the rest.
    pub fn for_path(path: &str) -> Self {
        const PACKED: [&str; 12] = [
            "mp4", "webm", "mkv", "png", "jpg", "jpeg", "gif", "webp", "ktx2", "woff2", "ogg", "mp3",
        ];
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        if ext.is_some_and(|e| PACKED.contains(&e.as_str())) {
            Compression::None
        } else {
            Compression::Lz4
        }
    }
}

/// Assembles a package. Entries are written in the order they were added.
pub struct Builder {
    manifest: Manifest,
    entries: Vec<(String, Vec<u8>, Compression)>,
    seen: HashSet<String>,
}

/// What `Builder::write` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub entries: usize,
    pub bytes: u64,
}

impl Builder {
    pub fn new(manifest: Manifest) -> Self {
        Builder {
            manifest,
            entries: Vec::new(),
            seen: HashSet::new(),
        }
    }

    pub fn add(
        &mut self,
        path: impl Into<String>,
        bytes: Vec<u8>,
        compression: Compression,
    ) -> Result<(), PackError> {
        let path = path.into();
        check_entry_path(&path)?;
        if !self.seen.insert(path.clone()) {
            return Err(PackError::DuplicatePath(path));
        }
        self.entries.push((path, bytes, compression));
        Ok(())
    }

    /// Validate the manifest against the entries, then write the package.
    /// The same inputs always give the same bytes, so a republished
    /// wallpaper that did not change hashes the same.
    pub fn write<W: Write + Seek>(self, out: &mut W) -> Result<Summary, PackError> {
        self.manifest
            .validate(self.entries.iter().map(|(p, _, _)| p.as_str()))?;

        out.seek(SeekFrom::Start(0))?;
        out.write_all(&[0u8; ALIGN as usize])?;
        let mut pos = ALIGN;
        let mut index = Index {
            entries: Vec::with_capacity(self.entries.len()),
        };

        for (path, bytes, compression) in &self.entries {
            let (stored, how) = match compression {
                Compression::Lz4 => {
                    let packed = lz4_flex::compress(bytes);
                    if (packed.len() as u64) * 8 <= (bytes.len() as u64) * 7 {
                        (std::borrow::Cow::Owned(packed), "lz4")
                    } else {
                        (std::borrow::Cow::Borrowed(bytes.as_slice()), "none")
                    }
                }
                Compression::None => (std::borrow::Cow::Borrowed(bytes.as_slice()), "none"),
            };
            out.write_all(&stored)?;
            let stored_len = stored.len() as u64;
            index.entries.push(Entry {
                path: path.clone(),
                offset: pos,
                stored_len,
                len: bytes.len() as u64,
                compression: how.to_owned(),
                cipher: "none".to_owned(),
                blake3: blake3::hash(&stored).to_hex().to_string(),
            });
            let end = pos + stored_len;
            let next = align_up(end);
            write_zeros(out, next - end)?;
            pos = next;
        }

        let manifest = self.manifest.to_json();
        let index = serde_json::to_vec(&index).expect("an index always serializes");
        out.write_all(&manifest)?;
        out.write_all(&index)?;
        let manifest_offset = pos;
        let index_offset = manifest_offset + manifest.len() as u64;
        let total = index_offset + index.len() as u64;

        let mut header = [0u8; HEADER_LEN];
        header[0..8].copy_from_slice(&MAGIC);
        header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[16..24].copy_from_slice(&manifest_offset.to_le_bytes());
        header[24..32].copy_from_slice(&(manifest.len() as u64).to_le_bytes());
        header[32..40].copy_from_slice(&index_offset.to_le_bytes());
        header[40..48].copy_from_slice(&(index.len() as u64).to_le_bytes());
        header[48..64].copy_from_slice(&directory_hash(&manifest, &index));
        out.seek(SeekFrom::Start(0))?;
        out.write_all(&header)?;
        out.seek(SeekFrom::Start(total))?;
        out.flush()?;

        Ok(Summary {
            entries: self.entries.len(),
            bytes: total,
        })
    }
}

fn write_zeros<W: Write>(out: &mut W, mut n: u64) -> std::io::Result<()> {
    const ZEROS: [u8; 4096] = [0; 4096];
    while n > 0 {
        let step = n.min(ZEROS.len() as u64) as usize;
        out.write_all(&ZEROS[..step])?;
        n -= step as u64;
    }
    Ok(())
}

/// Turn a folder into a package: `kirie.json` in it is the manifest, every
/// other file becomes an entry under its path relative to the folder.
/// Files and folders whose names start with `.`, and `.kpk` files, are left
/// out.
pub fn pack_dir<W: Write + Seek>(dir: &Path, out: &mut W) -> Result<Summary, PackError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let text = std::fs::read(&manifest_path).map_err(|source| PackError::File {
        path: manifest_path,
        source,
    })?;
    let manifest = Manifest::from_json_strict(&text)?;

    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();

    let mut builder = Builder::new(manifest);
    for (name, path) in files {
        if name == MANIFEST_FILE {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|source| PackError::File {
            path: path.clone(),
            source,
        })?;
        let compression = Compression::for_path(&name);
        builder.add(name, bytes, compression)?;
    }
    builder.write(out)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), PackError> {
    let read = std::fs::read_dir(dir).map_err(|source| PackError::File {
        path: dir.to_owned(),
        source,
    })?;
    for item in read {
        let item = item?;
        let path = item.path();
        let file_name = item.file_name().to_string_lossy().into_owned();
        // Hidden files are version control and editor litter. Packages are
        // skipped too: the one being written may be inside the folder.
        if file_name.starts_with('.') || file_name.ends_with(".kpk") || file_name.ends_with(".kpk.partial") {
            continue;
        }
        let kind = item.file_type()?;
        if kind.is_dir() {
            collect(root, &path, out)?;
        } else if kind.is_file() {
            let rel = path.strip_prefix(root).expect("walked from root");
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| PackError::BadPath {
                    path: rel.display().to_string(),
                    why: "it is not UTF-8",
                })?
                .join("/");
            out.push((name, path));
        }
    }
    Ok(())
}
