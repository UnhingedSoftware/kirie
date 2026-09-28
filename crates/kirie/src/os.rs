//! The handful of things kirie has to do differently per operating system.
//!
//! Kept together so that adding a platform means reading one file rather than
//! grepping for `std::os`.

use std::path::{Path, PathBuf};

/// The directory the control socket lives in.
///
/// It has to be private to one user and stable across runs, because haru
/// computes the same path from its own side and the two have to agree.
///
/// On Linux that is `XDG_RUNTIME_DIR`, which the session manager already makes
/// 0700 and per-user. Without one -- macOS, or a bare login -- the temp
/// directory is shared between accounts, so a fixed name collides and the
/// second user cannot even unlink the first user's socket under /tmp's sticky
/// bit. Give every uid its own 0700 directory instead.
///
/// `None` when no directory could be made private: one someone else controls
/// would let them swap the socket out from under us.
#[cfg(unix)]
pub(crate) fn runtime_dir() -> Option<PathBuf> {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(runtime));
    }
    let dir = std::env::temp_dir().join(format!("kirie-{}", current_user_tag()));
    match make_private_dir(&dir) {
        Ok(()) => Some(dir),
        Err(err) => {
            // A private one of our own is safer, even though haru, which
            // computes the shared name, will not find the socket there.
            tracing::error!(path = %dir.display(), %err, "the control-socket directory is not private");
            fresh_private_dir()
        }
    }
}

#[cfg(unix)]
fn fresh_private_dir() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "kirie-{}-{}-{stamp}",
        current_user_tag(),
        std::process::id()
    ));
    if let Err(err) = std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        tracing::error!(path = %dir.display(), %err, "cannot make a private control-socket directory");
        return None;
    }
    tracing::warn!(path = %dir.display(), "using a private control-socket directory of this process's own");
    Some(dir)
}

/// Make `dir` 0700, or check that the one already there is ours and make it
/// 0700. Anyone can create a name in the shared temp dir first, and a
/// directory someone else owns lets them replace the socket inside it.
#[cfg(unix)]
fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        other => return other,
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || own_uid().is_some_and(|uid| uid != meta.uid()) {
        return Err(std::io::Error::other(
            "it belongs to another account, or is not a directory",
        ));
    }
    // Only the owner may chmod, so this also proves ownership when no uid
    // could be read.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Windows has no `XDG_RUNTIME_DIR` and no mode bits to set, but it does have a
/// per-user directory the OS already keeps other accounts out of.
/// `%LOCALAPPDATA%\kirie` is that directory; the temp fallback matches the Unix
/// one for the case where the variable is missing.
#[cfg(windows)]
pub(crate) fn runtime_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        Some(local) => PathBuf::from(local).join("kirie"),
        None => std::env::temp_dir().join(format!("kirie-{}", current_user_tag())),
    };
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Something that differs between accounts on this machine, for naming a
/// directory only one of them should use.
#[cfg(unix)]
pub(crate) fn current_user_tag() -> String {
    own_uid().map_or_else(named_user, |uid| uid.to_string())
}

/// This account's uid, read off its home directory: the crate forbids the
/// `unsafe` a `getuid` call would need.
#[cfg(unix)]
fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;

    let home = std::env::var_os("HOME")?;
    std::fs::metadata(home).ok().map(|meta| meta.uid())
}

#[cfg(windows)]
pub(crate) fn current_user_tag() -> String {
    named_user()
}

fn named_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "shared".to_owned())
}

/// The per-user configuration directory: `XDG_CONFIG_HOME` or `~/.config`
/// on Unix, `%APPDATA%` on Windows, which sets neither `XDG_CONFIG_HOME` nor
/// `HOME`.
#[cfg(unix)]
pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
}

#[cfg(windows)]
pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Mark a file we just downloaded as something that can be run.
///
/// Windows decides that from the extension, so there is nothing to set; the
/// caller has already given the download its `.exe`.
#[cfg(unix)]
pub(crate) fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(windows)]
pub(crate) fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_directory_is_per_user() {
        // Either it is the session's own runtime directory, or it is a name
        // nobody else's account would land on.
        let dir = runtime_dir().expect("a private runtime directory");
        let tagged = dir.to_string_lossy().contains(&current_user_tag());
        let session_owned =
            std::env::var_os("XDG_RUNTIME_DIR").is_some() || std::env::var_os("LOCALAPPDATA").is_some();
        assert!(tagged || session_owned, "{}", dir.display());
    }

    #[test]
    fn a_user_tag_is_never_empty() {
        assert!(!current_user_tag().is_empty());
    }
}
