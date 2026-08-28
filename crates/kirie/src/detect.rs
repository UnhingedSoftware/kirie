use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Project,
    Pkg,
    Tex,
}

pub fn detect(path: &Path, bytes: &[u8]) -> Option<FileKind> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.ends_with(".json") {
        return Some(FileKind::Project);
    }
    if name.ends_with(".pkg") {
        return Some(FileKind::Pkg);
    }
    if name.ends_with(".tex") {
        return Some(FileKind::Tex);
    }
    if bytes.get(4..8) == Some(b"PKGV".as_slice()) {
        return Some(FileKind::Pkg);
    }
    if bytes.get(..4) == Some(b"TEXV".as_slice()) {
        return Some(FileKind::Tex);
    }
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') {
        return Some(FileKind::Project);
    }
    None
}
