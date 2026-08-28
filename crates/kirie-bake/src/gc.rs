use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::BakeError;

pub const DEFAULT_CAP_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const ATIME_FILE: &str = ".atime";
const BUNDLE_FILE: &str = "bundle.rkyv";

#[derive(Debug, Clone)]
struct Entry {
    dir: PathBuf,
    size: u64,
    accessed: SystemTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub total_before: u64,
    pub total_after: u64,
    pub evicted: usize,
    pub reclaimed: u64,
}

pub fn gc(bundles_dir: &Path, cap_bytes: u64) -> Result<GcReport, BakeError> {
    let mut entries = match scan(bundles_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GcReport::default()),
        Err(e) => return Err(BakeError::io(bundles_dir, e)),
    };

    let total_before: u64 = entries.iter().map(|e| e.size).sum();
    let mut total = total_before;
    let mut report = GcReport {
        total_before,
        total_after: total_before,
        ..Default::default()
    };
    if total <= cap_bytes {
        return Ok(report);
    }

    entries.sort_by_key(|e| e.accessed);
    for e in entries {
        if total <= cap_bytes {
            break;
        }
        match fs::remove_dir_all(&e.dir) {
            Ok(()) => {
                total = total.saturating_sub(e.size);
                report.evicted += 1;
                report.reclaimed += e.size;
            }
            Err(err) => {
                tracing::warn!(dir = %e.dir.display(), error = %err, "gc: eviction failed");
            }
        }
    }
    report.total_after = total;
    Ok(report)
}

fn scan(bundles_dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for dent in fs::read_dir(bundles_dir)? {
        let dent = dent?;
        let dir = dent.path();
        if !dir.is_dir() {
            continue;
        }
        let size = dir_size(&dir);
        let accessed = access_time(&dir);
        out.push(Entry { dir, size, accessed });
    }
    Ok(out)
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for dent in rd.flatten() {
            if let Ok(md) = dent.metadata()
                && md.is_file()
            {
                total += md.len();
            }
        }
    }
    total
}

fn access_time(dir: &Path) -> SystemTime {
    for name in [ATIME_FILE, BUNDLE_FILE] {
        if let Ok(md) = fs::metadata(dir.join(name))
            && let Ok(t) = md.modified()
        {
            return t;
        }
    }
    SystemTime::UNIX_EPOCH
}
