use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use kirie_launcher::{KEY_LEN, MAGIC, TRAILER_LEN};

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("kirie: {err}");
            std::process::ExitCode::from(127)
        }
    }
}

fn run() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let dir = ensure_extracted(&exe)?;
    let target = dir.join(if cfg!(windows) { "kirie.exe" } else { "kirie" });
    hand_over(&target)
}

/// Become the extracted engine.
///
/// Unix replaces this process with it, so what the session started keeps its
/// pid and its place in the job -- a wallpaper renderer being supervised by
/// systemd or launchd wants that. Windows has no `exec`, so the stub stays
/// alive as a parent doing nothing but waiting and passing the exit code on.
#[cfg(unix)]
fn hand_over(target: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;

    let err = Command::new(target).args(std::env::args_os().skip(1)).exec();
    Err(io::Error::other(format!(
        "cannot exec extracted engine {}: {err}",
        target.display()
    )))
}

#[cfg(windows)]
fn hand_over(target: &Path) -> io::Result<()> {
    let status = Command::new(target)
        .args(std::env::args_os().skip(1))
        .status()
        .map_err(|err| {
            io::Error::other(format!(
                "cannot start extracted engine {}: {err}",
                target.display()
            ))
        })?;
    // A process killed by a signal has no exit code of its own; 127 is what
    // `main` already reports for "the engine did not run".
    std::process::exit(status.code().unwrap_or(127));
}

fn ensure_extracted(exe: &Path) -> io::Result<PathBuf> {
    let mut f = File::open(exe)?;
    let size = f.metadata()?.len();
    if size < TRAILER_LEN as u64 {
        return Err(io::Error::other(
            "executable too small to be a kirie self-extracting binary",
        ));
    }

    let mut trailer = [0u8; TRAILER_LEN];
    f.seek(SeekFrom::End(-(TRAILER_LEN as i64)))?;
    f.read_exact(&mut trailer)?;
    if &trailer[..8] != MAGIC {
        return Err(io::Error::other(
            "not a kirie self-extracting binary (bad trailer magic)",
        ));
    }
    let blob_len = u64::from_le_bytes(trailer[8..16].try_into().unwrap());
    let key = std::str::from_utf8(&trailer[16..16 + KEY_LEN])
        .map_err(|_| io::Error::other("bad trailer cache key"))?;
    if !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        // The key names a directory under the cache root, so a truncated
        // download that still happens to carry the magic must not be able to
        // make that name `..` and put the extraction somewhere else.
        return Err(io::Error::other("bad trailer cache key"));
    }
    // The length comes out of the file being read, so a truncated or edited
    // binary can claim a blob bigger than the file it is in. Subtracting that
    // unchecked wraps to an offset near u64::MAX and the seek that follows
    // reads nothing useful; say what is actually wrong instead.
    let blob_off = (size - TRAILER_LEN as u64).checked_sub(blob_len).ok_or_else(|| {
        io::Error::other("kirie self-extracting binary is truncated (payload longer than the file)")
    })?;

    let root = cache_root()?;
    let dir = root.join(key);
    if dir.join(".complete").is_file() {
        return Ok(dir);
    }

    fs::create_dir_all(&root)?;
    let tmp = root.join(format!(".tmp.{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;

    f.seek(SeekFrom::Start(blob_off))?;
    let blob = BufReader::new(f.take(blob_len));
    let decoder = zstd::Decoder::new(blob)?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    archive.unpack(&tmp)?;
    File::create(tmp.join(".complete"))?;

    match fs::rename(&tmp, &dir) {
        Ok(()) => {
            prune_old_runtimes(&root, key);
            Ok(dir)
        }
        Err(_) if dir.join(".complete").is_file() => {
            let _ = fs::remove_dir_all(&tmp);
            Ok(dir)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

fn prune_old_runtimes(root: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            !n.starts_with('.') && n != keep
        })
        .filter_map(|e| {
            let m = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((e.path(), m))
        })
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.1));
    for (path, _) in dirs.into_iter().skip(1) {
        let _ = fs::remove_dir_all(path);
    }
}

/// Where the runtime this binary carries gets unpacked.
///
/// Windows sets neither `XDG_CACHE_HOME` nor `HOME`, so asking for those found
/// nothing and the launcher gave up before it extracted anything.
/// `%LOCALAPPDATA%` is the same idea under a different name.
#[cfg(windows)]
fn cache_root() -> io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))?;
    Ok(base.join("kirie").join("rt"))
}

#[cfg(unix)]
fn cache_root() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| io::Error::other("neither XDG_CACHE_HOME nor HOME is set"))?;
    Ok(base.join("kirie").join("rt"))
}
