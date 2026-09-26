use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    ALIGN, FORMAT_VERSION, HEADER_LEN, MAGIC, MAX_INDEX_LEN, MAX_MANIFEST_LEN, MAX_UNPACKED_ENTRY_LEN,
    Manifest, PackError, check_entry_path, directory_hash,
};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Index {
    pub(crate) entries: Vec<Entry>,
}

/// One file inside a package, as the index describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    /// Where the stored bytes start in the package file.
    pub offset: u64,
    pub stored_len: u64,
    /// Length once decompressed.
    pub len: u64,
    /// `none` or `lz4`.
    pub compression: String,
    /// `none` in v1; reserved for encrypted packages.
    #[serde(default = "none")]
    pub cipher: String,
    /// blake3 of the stored bytes, lowercase hex.
    pub blake3: String,
}

fn none() -> String {
    "none".to_owned()
}

/// Where an entry's bytes sit in the package file, for entries stored as is.
/// A player can hand this range to a decoder instead of copying the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub offset: u64,
    pub len: u64,
}

/// An opened package. The header, manifest and index are read and checked
/// on open; entries are read and verified on demand.
pub struct Package<R> {
    reader: R,
    manifest: Manifest,
    entries: Vec<Entry>,
    by_path: HashMap<String, usize>,
}

impl Package<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self, PackError> {
        let file = File::open(path).map_err(|source| PackError::File {
            path: path.to_owned(),
            source,
        })?;
        Package::from_reader(BufReader::new(file))
    }
}

impl<R: Read + Seek> Package<R> {
    pub fn from_reader(mut reader: R) -> Result<Self, PackError> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_LEN];
        if file_len < HEADER_LEN as u64 {
            return Err(PackError::NotAPackage);
        }
        reader.read_exact(&mut header)?;
        if header[0..8] != MAGIC {
            return Err(PackError::NotAPackage);
        }
        let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
        let u64_at = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().expect("8 bytes"));
        let version = u16_at(8);
        if version != FORMAT_VERSION {
            return Err(PackError::UnsupportedVersion(version));
        }
        let flags = u16_at(10);
        if flags != 0 {
            return Err(PackError::UnsupportedFlags(flags));
        }
        let (manifest_offset, manifest_len) = (u64_at(16), u64_at(24));
        let (index_offset, index_len) = (u64_at(32), u64_at(40));
        if manifest_len > MAX_MANIFEST_LEN || index_len > MAX_INDEX_LEN {
            return Err(PackError::corrupt("the manifest or index is implausibly large"));
        }
        if manifest_offset < ALIGN
            || index_offset != manifest_offset + manifest_len
            || index_offset.checked_add(index_len) != Some(file_len)
        {
            return Err(PackError::corrupt("the header's offsets do not fit the file"));
        }

        let manifest_bytes = read_at(&mut reader, manifest_offset, manifest_len)?;
        let index_bytes = read_at(&mut reader, index_offset, index_len)?;
        if directory_hash(&manifest_bytes, &index_bytes) != header[48..64] {
            return Err(PackError::corrupt(
                "the manifest or index does not match the header's hash",
            ));
        }
        let manifest = Manifest::from_json(&manifest_bytes)?;
        let index: Index = serde_json::from_slice(&index_bytes)
            .map_err(|e| PackError::corrupt(format!("unreadable index: {e}")))?;

        let mut by_path = HashMap::with_capacity(index.entries.len());
        let mut spans: Vec<(u64, u64)> = Vec::with_capacity(index.entries.len());
        for (i, e) in index.entries.iter().enumerate() {
            check_entry_path(&e.path)?;
            if by_path.insert(e.path.clone(), i).is_some() {
                return Err(PackError::DuplicatePath(e.path.clone()));
            }
            let end = e.offset.checked_add(e.stored_len);
            if e.offset < ALIGN || e.offset % ALIGN != 0 || end.is_none_or(|end| end > manifest_offset) {
                return Err(PackError::corrupt(format!(
                    "entry {:?} lies outside the entry area",
                    e.path
                )));
            }
            if e.compression == "none" && e.len != e.stored_len {
                return Err(PackError::corrupt(format!(
                    "entry {:?} has two different lengths",
                    e.path
                )));
            }
            if e.blake3.len() != 64 {
                return Err(PackError::corrupt(format!(
                    "entry {:?} has no usable hash",
                    e.path
                )));
            }
            spans.push((e.offset, e.stored_len));
        }
        spans.sort_unstable();
        if spans.windows(2).any(|w| w[0].0 + w[0].1 > w[1].0) {
            return Err(PackError::corrupt("two entries overlap"));
        }

        manifest.validate(index.entries.iter().map(|e| e.path.as_str()))?;
        Ok(Package {
            reader,
            manifest,
            entries: index.entries,
            by_path,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, path: &str) -> Option<&Entry> {
        self.by_path.get(path).map(|&i| &self.entries[i])
    }

    /// Read an entry, check its hash and decompress it.
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, PackError> {
        let entry = self
            .entry(path)
            .ok_or_else(|| PackError::NoSuchEntry(path.to_owned()))?
            .clone();
        if entry.cipher != "none" {
            return Err(PackError::UnsupportedStorage {
                path: entry.path,
                what: format!("cipher {}", entry.cipher),
            });
        }
        let stored = read_at(&mut self.reader, entry.offset, entry.stored_len)?;
        if blake3::hash(&stored).to_hex().as_str() != entry.blake3 {
            return Err(PackError::HashMismatch { path: entry.path });
        }
        match entry.compression.as_str() {
            "none" => Ok(stored),
            "lz4" => {
                if entry.len > MAX_UNPACKED_ENTRY_LEN {
                    return Err(PackError::corrupt(format!(
                        "entry {:?} claims to unpack to {} bytes",
                        entry.path, entry.len
                    )));
                }
                let bytes = lz4_flex::decompress(&stored, entry.len as usize)
                    .map_err(|e| PackError::corrupt(format!("entry {:?}: {e}", entry.path)))?;
                if bytes.len() as u64 != entry.len {
                    return Err(PackError::corrupt(format!(
                        "entry {:?} unpacked to the wrong length",
                        entry.path
                    )));
                }
                Ok(bytes)
            }
            other => Err(PackError::UnsupportedStorage {
                path: entry.path,
                what: format!("compression {other}"),
            }),
        }
    }

    /// Where an entry stored as is lies in the file. `None` for a missing,
    /// compressed or encrypted entry. The bytes in the range are not checked;
    /// call `verify` first if the file may have changed since it was opened.
    pub fn span(&self, path: &str) -> Option<Span> {
        let e = self.entry(path)?;
        (e.compression == "none" && e.cipher == "none").then_some(Span {
            offset: e.offset,
            len: e.stored_len,
        })
    }

    /// Check every entry's hash.
    pub fn verify(&mut self) -> Result<(), PackError> {
        let paths: Vec<String> = self.entries.iter().map(|e| e.path.clone()).collect();
        for path in paths {
            self.read(&path)?;
        }
        Ok(())
    }
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, len: u64) -> Result<Vec<u8>, PackError> {
    let len =
        usize::try_from(len).map_err(|_| PackError::corrupt("an entry is too large for this machine"))?;
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => PackError::corrupt("the file ends early"),
        _ => PackError::Io(e),
    })?;
    Ok(buf)
}
