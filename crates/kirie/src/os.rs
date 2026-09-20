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
#[cfg(unix)]
pub(crate) fn runtime_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime);
    }
    let dir = std::env::temp_dir().join(format!("kirie-{}", current_user_tag()));
    if std::fs::create_dir_all(&dir).is_ok() {
        use std::os::unix::fs::PermissionsExt;

        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Windows has no `XDG_RUNTIME_DIR` and no mode bits to set, but it does have a
/// per-user directory the OS already keeps other accounts out of.
/// `%LOCALAPPDATA%\kirie` is that directory; the temp fallback matches the Unix
/// one for the case where the variable is missing.
#[cfg(windows)]
pub(crate) fn runtime_dir() -> PathBuf {
    let dir = match std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        Some(local) => PathBuf::from(local).join("kirie"),
        None => std::env::temp_dir().join(format!("kirie-{}", current_user_tag())),
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Something that differs between accounts on this machine, for naming a
/// directory only one of them should use.
#[cfg(unix)]
pub(crate) fn current_user_tag() -> String {
    use std::os::unix::fs::MetadataExt;

    if let Some(home) = std::env::var_os("HOME")
        && let Ok(meta) = std::fs::metadata(&home)
    {
        return meta.uid().to_string();
    }
    named_user()
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
        let dir = runtime_dir();
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
