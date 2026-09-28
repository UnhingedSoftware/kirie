//! The places where the control socket has to know which operating system it
//! is on.
//!
//! They are all here rather than spread through the protocol code: the socket
//! type itself, who may connect to it, how a path crosses the wire, and what a
//! socket someone else left behind is to us.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};

// Windows has had AF_UNIX since Windows 10 1803, but the standard library
// exposes it only under `std::os::unix`. `uds_windows` is the same sockets
// with the same API, so everything above this line stays as it was and the
// socket stays a path on disk -- which matters, because haru computes that
// path itself rather than asking us for it.
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

/// Bind a listener at `path` that only this account can connect to.
///
/// Whoever can connect can load any wallpaper and write a screenshot anywhere
/// this account can write, so the socket must not be left at whatever the
/// umask allows. There is a moment between the bind and the chmod, which is
/// why the socket belongs in a directory only its owner can enter.
pub fn bind_private(path: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    if let Err(err) = restrict_to_owner(path) {
        let _ = std::fs::remove_file(path);
        return Err(err);
    }
    Ok(listener)
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// A Windows socket file takes its ACL from the directory it is made in, and
/// the default one, `%LOCALAPPDATA%\kirie`, is already private to the account.
#[cfg(windows)]
fn restrict_to_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Remove the socket a previous run left at `path`, and nothing else.
///
/// The path is typed by a person (`--control-socket`, `--socket`), and a typo
/// that names a real file must not delete it; the bind that follows fails
/// instead. A missing path is not an error.
#[cfg(unix)]
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// std cannot tell an AF_UNIX socket file from a plain one on Windows, so
/// this removes whatever is there, as it always has.
#[cfg(windows)]
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// A path as it goes onto the wire.
///
/// The protocol is bytes, because on Linux that is what a path is: whatever the
/// caller typed, valid UTF-8 or not, has to reach the renderer unchanged.
#[cfg(unix)]
pub fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;

    Cow::Borrowed(path.as_os_str().as_bytes())
}

/// A Windows path is UTF-16, so it has no byte view to borrow and nothing is
/// lost by sending it as UTF-8. The lossy conversion only bites on unpaired
/// surrogates, which no path from the filesystem carries.
#[cfg(windows)]
pub fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    match path.to_string_lossy() {
        Cow::Borrowed(text) => Cow::Borrowed(text.as_bytes()),
        Cow::Owned(text) => Cow::Owned(text.into_bytes()),
    }
}

#[cfg(unix)]
pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;

    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(windows)]
pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Whether these bytes read as a path rather than a screen name.
///
/// `bg` takes either `bg <path>` or `bg <screen> <path>`, so the parser has to
/// guess which it was handed. A leading `/` or `~/` settles it on Unix; on
/// Windows the same job is done by a drive letter, a UNC prefix, or a
/// backslash.
pub(crate) fn looks_like_a_path(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"/") || bytes.starts_with(b"~/") {
        return true;
    }
    if cfg!(windows) && (starts_with_a_drive(bytes) || bytes.starts_with(b"\\")) {
        return true;
    }
    Path::new(&path_from_bytes(bytes)).exists()
}

fn starts_with_a_drive(bytes: &[u8]) -> bool {
    matches!(bytes, [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic())
}

/// Whether a socket already sitting at this path was made by somebody else.
///
/// A socket we did not create is one we must not delete, and one we should not
/// talk to either.
#[cfg(unix)]
pub(crate) fn socket_is_foreign(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(socket) = std::fs::metadata(path) else {
        return false;
    };
    let ours = std::env::var_os("HOME")
        .and_then(|home| std::fs::metadata(home).ok())
        .map(|home| home.uid());
    ours.is_some_and(|uid| uid != socket.uid())
}

/// Windows has no uid to compare, and no cheap equivalent: the owner is a SID
/// behind a security descriptor. What stands in for the check is where the
/// socket lives -- `runtime_dir()` puts it under the user's own
/// `%LOCALAPPDATA%`, which another account cannot write to.
#[cfg(windows)]
pub(crate) fn socket_is_foreign(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_private_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("kirie-ipc-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("private.sock");
        let _listener = bind_private(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_socket_is_removed_as_stale() {
        let dir = std::env::temp_dir().join(format!("kirie-ipc-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"keep me").unwrap();
        let socket = dir.join("old.sock");
        drop(UnixListener::bind(&socket).unwrap());

        remove_stale_socket(&file).unwrap();
        remove_stale_socket(&socket).unwrap();
        remove_stale_socket(&dir.join("missing.sock")).unwrap();
        let (file_kept, socket_gone) = (file.exists(), !socket.exists());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(file_kept);
        assert!(socket_gone);
    }

    #[test]
    fn a_path_survives_the_wire() {
        let path = PathBuf::from(if cfg!(windows) {
            r"C:\Users\someone\wallpapers\1388331347"
        } else {
            "/home/someone/wallpapers/1388331347"
        });
        assert_eq!(path_from_bytes(&path_bytes(&path)), path);
    }

    #[test]
    fn a_path_with_spaces_survives_too() {
        let path = PathBuf::from(if cfg!(windows) {
            r"C:\Users\someone\My Wallpapers\1388331347"
        } else {
            "/home/someone/My Wallpapers/1388331347"
        });
        assert_eq!(path_from_bytes(&path_bytes(&path)), path);
    }

    #[test]
    fn an_absolute_path_reads_as_one() {
        assert!(looks_like_a_path(b"/home/someone/wallpapers/1388331347"));
        assert!(looks_like_a_path(b"~/wallpapers/1388331347"));
    }

    #[test]
    fn a_screen_name_does_not() {
        assert!(!looks_like_a_path(b"DP-1"));
        assert!(!looks_like_a_path(b"Built-in Retina Display"));
    }

    #[test]
    fn a_windows_path_reads_as_one_on_windows() {
        assert_eq!(looks_like_a_path(br"C:\Users\someone\wall"), cfg!(windows));
        assert_eq!(looks_like_a_path(br"\\server\share\wall"), cfg!(windows));
    }

    #[test]
    fn a_drive_letter_needs_a_separator_after_it() {
        assert!(!starts_with_a_drive(b"C:"));
        assert!(!starts_with_a_drive(b"screen:1"));
        assert!(starts_with_a_drive(br"C:\wall"));
        assert!(starts_with_a_drive(b"C:/wall"));
    }
}
