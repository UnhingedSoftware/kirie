use std::ops::Range;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PkgError {
    #[error("expected magic to start with \"PKGV\", got {found:?}")]
    BadMagic { found: String },

    #[error(
        "truncated package: need {needed} byte(s) for {what} at offset {offset}, \
         only {available} available"
    )]
    Truncated {
        what: &'static str,
        offset: usize,
        needed: usize,
        available: usize,
    },

    #[error(
        "entry {name:?} payload out of bounds: base offset {base_offset} + entry offset \
         {offset} + length {length} exceeds package size {package_size}"
    )]
    PayloadOutOfBounds {
        name: String,
        base_offset: usize,
        offset: u32,
        length: u32,
        package_size: usize,
    },

    #[error("no entry named {name:?} in package")]
    EntryNotFound { name: String },

    #[error("failed to read package file `{}`: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a [u8],
    pub offset: u32,
    pub len: u32,
}

impl<'a> Entry<'a> {
    #[must_use]
    pub fn name_str(&self) -> Option<&'a str> {
        std::str::from_utf8(self.name).ok()
    }
}

#[derive(Debug, Clone)]
pub struct Pkg<'a> {
    data: &'a [u8],
    magic: &'a [u8],
    base_offset: usize,
    entries: Vec<Entry<'a>>,
}

impl<'a> Pkg<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, PkgError> {
        let raw = parse_raw(data)?;
        let entries = raw
            .entries
            .iter()
            .map(|e| Entry {
                name: slice(data, &e.name),
                offset: e.offset,
                len: e.len,
            })
            .collect();
        Ok(Self {
            data,
            magic: slice(data, &raw.magic),
            base_offset: raw.base_offset,
            entries,
        })
    }

    #[must_use]
    pub fn magic(&self) -> &'a [u8] {
        self.magic
    }

    #[must_use]
    pub fn version(&self) -> &'a [u8] {
        self.magic.get(4..).unwrap_or(&[])
    }

    #[must_use]
    pub fn base_offset(&self) -> usize {
        self.base_offset
    }

    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry<'a>] {
        &self.entries
    }

    #[must_use]
    pub fn get(&self, name: &[u8]) -> Option<Entry<'a>> {
        self.entries.iter().find(|e| e.name == name).copied()
    }

    pub fn read(&self, entry: &Entry<'_>) -> Result<&'a [u8], PkgError> {
        read_payload(self.data, self.base_offset, entry.name, entry.offset, entry.len)
    }

    pub fn read_name(&self, name: &[u8]) -> Result<&'a [u8], PkgError> {
        let entry = self.get(name).ok_or_else(|| PkgError::EntryNotFound {
            name: String::from_utf8_lossy(name).into_owned(),
        })?;
        self.read(&entry)
    }
}

#[derive(Debug, Clone)]
pub struct OwnedPkg {
    data: PkgData,
    raw: RawPkg,
}

enum PkgData {
    Vec(Vec<u8>),
    External(Box<dyn AsRef<[u8]> + Send + Sync>),
}

impl PkgData {
    fn as_slice(&self) -> &[u8] {
        match self {
            PkgData::Vec(v) => v,
            PkgData::External(b) => (**b).as_ref(),
        }
    }
}

impl std::fmt::Debug for PkgData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PkgData({} bytes)", self.as_slice().len())
    }
}

impl Clone for PkgData {
    fn clone(&self) -> Self {
        PkgData::Vec(self.as_slice().to_vec())
    }
}

impl OwnedPkg {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PkgError> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|source| PkgError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_vec(data)
    }

    pub fn from_vec(data: Vec<u8>) -> Result<Self, PkgError> {
        let raw = parse_raw(&data)?;
        Ok(Self {
            data: PkgData::Vec(data),
            raw,
        })
    }

    pub fn from_external(data: Box<dyn AsRef<[u8]> + Send + Sync>) -> Result<Self, PkgError> {
        let raw = parse_raw((*data).as_ref())?;
        Ok(Self {
            data: PkgData::External(data),
            raw,
        })
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    #[must_use]
    pub fn magic(&self) -> &[u8] {
        slice(self.data.as_slice(), &self.raw.magic)
    }

    #[must_use]
    pub fn version(&self) -> &[u8] {
        self.magic().get(4..).unwrap_or(&[])
    }

    #[must_use]
    pub fn base_offset(&self) -> usize {
        self.raw.base_offset
    }

    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.raw.entries.len()
    }

    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        self.raw.entries.iter().map(|e| Entry {
            name: slice(self.data.as_slice(), &e.name),
            offset: e.offset,
            len: e.len,
        })
    }

    #[must_use]
    pub fn get(&self, name: &[u8]) -> Option<Entry<'_>> {
        self.entries().find(|e| e.name == name)
    }

    pub fn read(&self, entry: &Entry<'_>) -> Result<&[u8], PkgError> {
        read_payload(
            self.data.as_slice(),
            self.raw.base_offset,
            entry.name,
            entry.offset,
            entry.len,
        )
    }

    pub fn read_name(&self, name: &[u8]) -> Result<&[u8], PkgError> {
        let entry = self.get(name).ok_or_else(|| PkgError::EntryNotFound {
            name: String::from_utf8_lossy(name).into_owned(),
        })?;
        read_payload(
            self.data.as_slice(),
            self.raw.base_offset,
            entry.name,
            entry.offset,
            entry.len,
        )
    }
}

#[derive(Debug, Clone)]
struct RawPkg {
    magic: Range<usize>,
    base_offset: usize,
    entries: Vec<RawEntry>,
}

#[derive(Debug, Clone)]
struct RawEntry {
    name: Range<usize>,
    offset: u32,
    len: u32,
}

fn parse_raw(data: &[u8]) -> Result<RawPkg, PkgError> {
    let mut r = Reader { data, pos: 0 };

    let magic = r.read_sstr("magic length", "magic bytes")?;
    let magic_bytes = slice(data, &magic);
    if !magic_bytes.starts_with(b"PKGV") {
        return Err(PkgError::BadMagic {
            found: String::from_utf8_lossy(magic_bytes).into_owned(),
        });
    }

    let count = r.read_u32("entry count")?;
    let remaining = data.len().saturating_sub(r.pos);
    let mut entries = Vec::with_capacity((count as usize).min(remaining / 12));
    for _ in 0..count {
        let name = r.read_sstr("entry name length", "entry name bytes")?;
        let offset = r.read_u32("entry offset")?;
        let len = r.read_u32("entry length")?;
        entries.push(RawEntry { name, offset, len });
    }

    Ok(RawPkg {
        magic,
        base_offset: r.pos,
        entries,
    })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize, what: &'static str) -> Result<&'a [u8], PkgError> {
        let truncated = || PkgError::Truncated {
            what,
            offset: self.pos,
            needed: len,
            available: self.data.len().saturating_sub(self.pos),
        };
        let end = self.pos.checked_add(len).ok_or_else(truncated)?;
        let bytes = self.data.get(self.pos..end).ok_or_else(truncated)?;
        self.pos = end;
        Ok(bytes)
    }

    fn read_u32(&mut self, what: &'static str) -> Result<u32, PkgError> {
        let offset = self.pos;
        let bytes = self.take(4, what)?;
        match bytes.first_chunk::<4>() {
            Some(arr) => Ok(u32::from_le_bytes(*arr)),
            None => Err(PkgError::Truncated {
                what,
                offset,
                needed: 4,
                available: bytes.len(),
            }),
        }
    }

    fn read_sstr(
        &mut self,
        what_len: &'static str,
        what_bytes: &'static str,
    ) -> Result<Range<usize>, PkgError> {
        let len = self.read_u32(what_len)?;
        let start = self.pos;
        self.take(len as usize, what_bytes)?;
        Ok(start..self.pos)
    }
}

fn slice<'d>(data: &'d [u8], range: &Range<usize>) -> &'d [u8] {
    data.get(range.clone()).unwrap_or(&[])
}

fn read_payload<'d>(
    data: &'d [u8],
    base_offset: usize,
    name: &[u8],
    offset: u32,
    length: u32,
) -> Result<&'d [u8], PkgError> {
    let oob = || PkgError::PayloadOutOfBounds {
        name: String::from_utf8_lossy(name).into_owned(),
        base_offset,
        offset,
        length,
        package_size: data.len(),
    };
    let start = (base_offset as u64)
        .checked_add(u64::from(offset))
        .ok_or_else(oob)?;
    let end = start.checked_add(u64::from(length)).ok_or_else(oob)?;
    if end > data.len() as u64 {
        return Err(oob());
    }
    let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
        return Err(oob());
    };
    data.get(start..end).ok_or_else(oob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sstr(s: &[u8]) -> Vec<u8> {
        let mut v = (s.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(s);
        v
    }

    fn build_pkg(magic: &[u8], entries: &[(&[u8], u32, u32)], payload: &[u8]) -> Vec<u8> {
        let mut v = sstr(magic);
        v.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (name, offset, len) in entries {
            v.extend_from_slice(&sstr(name));
            v.extend_from_slice(&offset.to_le_bytes());
            v.extend_from_slice(&len.to_le_bytes());
        }
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn happy_path_two_entries() {
        let data = build_pkg(
            b"PKGV0001",
            &[(b"scene.json", 0, 5), (b"a/b.txt", 5, 3)],
            b"helloabc",
        );
        let pkg = Pkg::parse(&data).unwrap();

        assert_eq!(pkg.magic(), b"PKGV0001");
        assert_eq!(pkg.version(), b"0001");
        assert_eq!(pkg.base_offset(), 57);
        assert_eq!(pkg.base_offset(), data.len() - 8);
        assert_eq!(pkg.entry_count(), 2);

        let e0 = pkg.entries()[0];
        assert_eq!(e0.name, b"scene.json");
        assert_eq!(e0.name_str(), Some("scene.json"));
        assert_eq!((e0.offset, e0.len), (0, 5));
        assert_eq!(pkg.read(&e0).unwrap(), b"hello");

        let e1 = pkg.get(b"a/b.txt").unwrap();
        assert_eq!((e1.offset, e1.len), (5, 3));
        assert_eq!(pkg.read(&e1).unwrap(), b"abc");
        assert_eq!(pkg.read_name(b"scene.json").unwrap(), b"hello");

        assert!(pkg.get(b"Scene.json").is_none());
        assert!(pkg.get(b"missing").is_none());
        assert!(matches!(
            pkg.read_name(b"missing"),
            Err(PkgError::EntryNotFound { .. })
        ));
    }

    #[test]
    fn empty_archive() {
        let data = build_pkg(b"PKGV0009", &[], b"");
        let pkg = Pkg::parse(&data).unwrap();
        assert_eq!(pkg.entry_count(), 0);
        assert_eq!(pkg.base_offset(), data.len());
        assert!(pkg.get(b"anything").is_none());
    }

    #[test]
    fn non_pkgv_magic_rejected() {
        let data = build_pkg(b"NOPE0001", &[], b"");
        assert!(matches!(
            Pkg::parse(&data),
            Err(PkgError::BadMagic { found }) if found == "NOPE0001"
        ));
        let data = build_pkg(b"PKG", &[], b"");
        assert!(matches!(Pkg::parse(&data), Err(PkgError::BadMagic { .. })));
        let data = build_pkg(b"", &[], b"");
        assert!(matches!(Pkg::parse(&data), Err(PkgError::BadMagic { .. })));
    }

    #[test]
    fn magic_prefix_rule_accepts_any_suffix_and_length() {
        for (magic, version) in [
            (b"PKGVabcd".as_slice(), b"abcd".as_slice()),
            (b"PKGV000123".as_slice(), b"000123".as_slice()),
            (b"PKGV".as_slice(), b"".as_slice()),
        ] {
            let data = build_pkg(magic, &[], b"");
            let pkg = Pkg::parse(&data).unwrap();
            assert_eq!(pkg.magic(), magic);
            assert_eq!(pkg.version(), version);
        }
    }

    #[test]
    fn truncated_header() {
        assert!(matches!(
            Pkg::parse(&[]),
            Err(PkgError::Truncated {
                what: "magic length",
                ..
            })
        ));
        assert!(matches!(
            Pkg::parse(&[0x08, 0x00]),
            Err(PkgError::Truncated {
                what: "magic length",
                ..
            })
        ));
        let mut data = 100u32.to_le_bytes().to_vec();
        data.extend_from_slice(b"PKGV0001");
        assert!(matches!(
            Pkg::parse(&data),
            Err(PkgError::Truncated {
                what: "magic bytes",
                ..
            })
        ));
        assert!(matches!(
            Pkg::parse(&sstr(b"PKGV0001")),
            Err(PkgError::Truncated {
                what: "entry count",
                ..
            })
        ));
    }

    #[test]
    fn truncated_table() {
        let full = build_pkg(b"PKGV0001", &[(b"a", 0, 1)], b"x");
        let mut data = full.clone();
        data[12..16].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            Pkg::parse(&data),
            Err(PkgError::Truncated {
                what: "entry name length",
                ..
            })
        ));

        let mut data = sstr(b"PKGV0001");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(b"ab");
        assert!(matches!(
            Pkg::parse(&data),
            Err(PkgError::Truncated {
                what: "entry name bytes",
                ..
            })
        ));

        let mut data = sstr(b"PKGV0001");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&sstr(b"scene.json"));
        assert!(matches!(
            Pkg::parse(&data),
            Err(PkgError::Truncated {
                what: "entry offset",
                ..
            })
        ));

        let mut data = sstr(b"PKGV0001");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&sstr(b"scene.json"));
        data.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            Pkg::parse(&data),
            Err(PkgError::Truncated {
                what: "entry length",
                ..
            })
        ));
    }

    #[test]
    fn hostile_entry_count_fails_fast_without_huge_alloc() {
        let mut data = sstr(b"PKGV0001");
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(Pkg::parse(&data), Err(PkgError::Truncated { .. })));
    }

    #[test]
    fn payload_out_of_range_is_typed_error() {
        let data = build_pkg(b"PKGV0001", &[(b"big", 0, 100)], b"hello");
        let pkg = Pkg::parse(&data).unwrap();
        let entry = pkg.get(b"big").unwrap();
        assert!(matches!(
            pkg.read(&entry),
            Err(PkgError::PayloadOutOfBounds { length: 100, .. })
        ));

        let data = build_pkg(b"PKGV0001", &[(b"z", 100, 0)], b"hello");
        let pkg = Pkg::parse(&data).unwrap();
        let entry = pkg.get(b"z").unwrap();
        assert!(matches!(
            pkg.read(&entry),
            Err(PkgError::PayloadOutOfBounds { .. })
        ));

        let data = build_pkg(b"PKGV0001", &[(b"end", 5, 0)], b"hello");
        let pkg = Pkg::parse(&data).unwrap();
        let entry = pkg.get(b"end").unwrap();
        assert_eq!(pkg.read(&entry).unwrap(), b"");

        let data = build_pkg(b"PKGV0001", &[(b"max", u32::MAX, u32::MAX)], b"hello");
        let pkg = Pkg::parse(&data).unwrap();
        let entry = pkg.get(b"max").unwrap();
        assert!(matches!(
            pkg.read(&entry),
            Err(PkgError::PayloadOutOfBounds { .. })
        ));
    }

    #[test]
    fn duplicate_names_first_occurrence_wins() {
        let data = build_pkg(b"PKGV0001", &[(b"dup", 0, 3), (b"dup", 3, 3)], b"aaabbb");
        let pkg = Pkg::parse(&data).unwrap();
        let entry = pkg.get(b"dup").unwrap();
        assert_eq!(pkg.read(&entry).unwrap(), b"aaa");
    }

    #[test]
    fn unordered_gapped_overlapping_entries_tolerated() {
        let data = build_pkg(
            b"PKGV0002",
            &[(b"late", 6, 2), (b"early", 0, 4), (b"overlap", 2, 4)],
            b"01234567",
        );
        let pkg = Pkg::parse(&data).unwrap();
        assert_eq!(pkg.read_name(b"late").unwrap(), b"67");
        assert_eq!(pkg.read_name(b"early").unwrap(), b"0123");
        assert_eq!(pkg.read_name(b"overlap").unwrap(), b"2345");
    }

    #[test]
    fn utf8_names_matched_byte_exactly() {
        let name = "models/背景.json".as_bytes();
        assert_eq!(name.len(), 18);
        let data = build_pkg(b"PKGV0022", &[(name, 0, 2)], b"{}");
        let pkg = Pkg::parse(&data).unwrap();
        let entry = pkg.get(name).unwrap();
        assert_eq!(entry.name, name);
        assert_eq!(pkg.read(&entry).unwrap(), b"{}");
    }

    #[test]
    fn entries_are_zero_copy_views_of_input() {
        let data = build_pkg(b"PKGV0001", &[(b"scene.json", 0, 5)], b"hello");
        let (entry, payload) = {
            let pkg = Pkg::parse(&data).unwrap();
            let entry = pkg.get(b"scene.json").unwrap();
            let payload = pkg.read(&entry).unwrap();
            (entry, payload)
        };
        assert_eq!(entry.name.as_ptr(), data[20..].as_ptr());
        assert_eq!(payload.as_ptr(), data[data.len() - 5..].as_ptr());
    }

    static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_file(bytes: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "kirie-pkg-test-{}-{}.pkg",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn owned_pkg_from_path_round_trip() {
        let bytes = build_pkg(
            b"PKGV0024",
            &[(b"scene.json", 0, 5), (b"a/b.txt", 5, 3)],
            b"helloabc",
        );
        let path = temp_file(&bytes);
        let pkg = OwnedPkg::from_path(&path).unwrap();
        assert_eq!(pkg.magic(), b"PKGV0024");
        assert_eq!(pkg.version(), b"0024");
        assert_eq!(pkg.entry_count(), 2);
        assert_eq!(pkg.base_offset(), bytes.len() - 8);
        assert_eq!(pkg.as_bytes(), bytes.as_slice());
        let names: Vec<Vec<u8>> = pkg.entries().map(|e| e.name.to_vec()).collect();
        assert_eq!(names, [b"scene.json".to_vec(), b"a/b.txt".to_vec()]);
        let entry = pkg.get(b"a/b.txt").unwrap();
        assert_eq!(pkg.read(&entry).unwrap(), b"abc");
        assert_eq!(pkg.read_name(b"scene.json").unwrap(), b"hello");
        assert!(matches!(
            pkg.read_name(b"missing"),
            Err(PkgError::EntryNotFound { .. })
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn owned_pkg_from_path_missing_file_is_io_error() {
        let err = OwnedPkg::from_path("/nonexistent/kirie-no-such-file.pkg").unwrap_err();
        assert!(matches!(err, PkgError::Io { .. }));
    }

    #[test]
    fn owned_pkg_from_vec_rejects_malformed() {
        assert!(matches!(
            OwnedPkg::from_vec(vec![]),
            Err(PkgError::Truncated { .. })
        ));
        assert!(matches!(
            OwnedPkg::from_vec(build_pkg(b"XXXX0001", &[], b"")),
            Err(PkgError::BadMagic { .. })
        ));
    }

    const CORPUS_DIR: &str = "/home/aiko/.steam/steam/steamapps/workshop/content/431960";
    const CORPUS_SCENE_PKG_COUNT: usize = 19;
    const CORPUS_TOTAL_ENTRIES: usize = 800;

    fn corpus_dir() -> Option<PathBuf> {
        let dir = std::env::var_os("KIRIE_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(CORPUS_DIR));
        if dir.is_dir() {
            Some(dir)
        } else {
            eprintln!(
                "skipping corpus test: {} not found (set KIRIE_CORPUS to override)",
                dir.display()
            );
            None
        }
    }

    fn corpus_scene_pkgs(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|item| item.path().join("scene.pkg"))
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn corpus_every_archive_parses_and_every_entry_reads() {
        let Some(dir) = corpus_dir() else { return };
        let paths = corpus_scene_pkgs(&dir);
        assert!(
            paths.len() >= CORPUS_SCENE_PKG_COUNT,
            "corpus scene.pkg count {} fell below docs/format-pkg.md §7 floor {CORPUS_SCENE_PKG_COUNT}",
            paths.len()
        );

        let mut total_entries = 0usize;
        for path in &paths {
            let pkg = OwnedPkg::from_path(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(pkg.magic().starts_with(b"PKGV"), "{}: bad magic", path.display());
            assert!(pkg.entry_count() > 0, "{}: no entries", path.display());
            total_entries += pkg.entry_count();
            for entry in pkg.entries() {
                let payload = pkg
                    .read(&entry)
                    .unwrap_or_else(|e| panic!("{}: entry {:?}: {e}", path.display(), entry.name_str()));
                assert_eq!(payload.len(), entry.len as usize);
            }
        }
        assert!(
            total_entries >= CORPUS_TOTAL_ENTRIES,
            "total corpus entry count {total_entries} fell below docs/format-pkg.md §7 floor {CORPUS_TOTAL_ENTRIES}"
        );
    }

    #[test]
    fn corpus_item_1388331347_matches_spec_hexdump() {
        let Some(dir) = corpus_dir() else { return };
        let path = dir.join("1388331347/scene.pkg");
        if !path.is_file() {
            eprintln!("skipping: {} not present", path.display());
            return;
        }
        let pkg = OwnedPkg::from_path(&path).unwrap();
        assert_eq!(pkg.as_bytes().len(), 4_124_099);
        assert_eq!(pkg.magic(), b"PKGV0001");
        assert_eq!(pkg.version(), b"0001");
        assert_eq!(pkg.entry_count(), 44);
        assert_eq!(pkg.base_offset(), 0x891);

        let e0 = pkg.entries().next().unwrap();
        assert_eq!(e0.name, b"shaders/effects/waterflow.vert");
        assert_eq!((e0.offset, e0.len), (0, 449));
        let payload = pkg.read(&e0).unwrap();
        assert!(payload.starts_with(b"\r\nuniform mat4 g_ModelViewProje"));

        let scene = pkg.get(b"scene.json").unwrap();
        assert_eq!((scene.offset, scene.len), (449, 5050));
        let payload = pkg.read(&scene).unwrap();
        assert!(payload.starts_with(b"{\r\n\t\"camera\""));
    }
}
