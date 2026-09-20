//! The places where the control socket has to know which operating system it
//! is on.
//!
//! There are three of them, and they are all here rather than spread through
//! the protocol code: the socket type itself, how a path crosses the wire, and
//! whether a socket someone else left behind belongs to us.

use std::borrow::Cow;
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
