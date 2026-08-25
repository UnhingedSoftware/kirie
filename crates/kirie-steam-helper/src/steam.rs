//! The Steamworks flat-C seam.
//!
//! Every `unsafe` in this workspace's Steam support lives here (SPEC §V2
//! exception). The rules it follows:
//!
//! * **Nothing of Valve's is redistributed.** The library is `dlopen`ed from
//!   the user's own Steam install and the signatures below are declared by hand
//!   from public documentation — no SDK headers, no vendored `.so`, no SDK
//!   agreement. This is also why `steamworks-rs` is not used: it vendors the
//!   SDK under Valve's own terms, which does not combine with this project's
//!   AGPL.
//! * **The connection is never held open.** Initialising as an app makes the
//!   Steam client treat this process as that app running. This is measured, not
//!   assumed: holding a session for 40 s moved the account's Wallpaper Engine
//!   `LastPlayed` to the second the session ended and incremented `Playtime`
//!   by a minute. A wallpaper daemon that held it would sit in "Playing
//!   Wallpaper Engine" forever and bill the user's play history for it. Each
//!   [`Session`] therefore does one job and shuts down, which is why this is a
//!   separate short-lived process rather than a module inside the engine — and
//!   why waiting for a download must be done by watching the filesystem, never
//!   by polling Steam.
//! * **The licence check is Steam's.** [`Session::open`] succeeds only when the
//!   signed-in account owns the app, so "does the user actually own Wallpaper
//!   Engine" is answered by Steam rather than trusted from our side.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::{Path, PathBuf};

/// Wallpaper Engine. The Workshop this talks to belongs to this app, and it is
/// the app the signed-in account must own.
pub const APP_ID: u32 = 431_960;

/// Where the Steam client keeps `libsteam_api.so`, relative to a Steam root.
///
/// Only the modern client layout ships it; the older `ubuntu12_*`/`linux64`
/// trees carry `steamclient.so` alone. Scavenging a copy out of some installed
/// game is not done — an absent library is reported as "unsupported Steam
/// install", not worked around.
const LIB_RELATIVE: [&str; 2] = ["steamrt64/libsteam_api.so", "steamrt32/libsteam_api.so"];

/// What went wrong, in terms a caller can act on.
#[derive(Debug)]
pub enum SteamError {
    /// No `libsteam_api.so` under any known Steam root.
    LibraryMissing,
    /// `dlopen`/`dlsym` failed, or a symbol this build needs is absent.
    LibraryUnusable(String),
    /// The Steam client is not running.
    NotRunning,
    /// Steam refused to initialise — most often because the signed-in account
    /// does not own the app.
    InitFailed(String),
}

impl std::fmt::Display for SteamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryMissing => write!(
                f,
                "no libsteam_api.so in this Steam install (needs a current Steam client)"
            ),
            Self::LibraryUnusable(why) => write!(f, "Steam library unusable: {why}"),
            Self::NotRunning => write!(f, "the Steam client is not running"),
            Self::InitFailed(why) => write!(f, "Steam refused to initialise: {why}"),
        }
    }
}

/// Locate `libsteam_api.so` under any of the given Steam roots.
#[must_use]
pub fn find_library(steam_roots: &[PathBuf]) -> Option<PathBuf> {
    steam_roots
        .iter()
        .flat_map(|root| LIB_RELATIVE.iter().map(move |rel| root.join(rel)))
        .find(|path| path.is_file())
}

/// A live Steamworks connection. Shuts down on drop.
pub struct Session {
    // Field order is the drop order: `shutdown` must run before the library is
    // unloaded, so the library is declared last.
    shutdown: unsafe extern "C" fn(),
    apps: *mut c_void,
    get_app_install_dir: unsafe extern "C" fn(*mut c_void, u32, *mut c_char, u32) -> u32,
    is_app_installed: unsafe extern "C" fn(*mut c_void, u32) -> bool,
    is_subscribed_app: unsafe extern "C" fn(*mut c_void, u32) -> bool,
    _library: libloading::Library,
}

impl Session {
    /// Connect to the running Steam client as [`APP_ID`].
    ///
    /// Fails when Steam is not running, when the account does not own the app,
    /// or when the client is too old to ship the library.
    pub fn open(steam_roots: &[PathBuf]) -> Result<Self, SteamError> {
        let path = find_library(steam_roots).ok_or(SteamError::LibraryMissing)?;
        Self::open_library(&path)
    }

    /// [`Self::open`] against one specific library path.
    pub fn open_library(path: &Path) -> Result<Self, SteamError> {
        // Steam reads the app identity from the environment (or a
        // `steam_appid.txt` in the working directory, which a daemon cannot
        // rely on). Set it before init; this process exists only to do that.
        //
        // SAFETY: single-threaded at this point — `main` has spawned nothing,
        // and no other thread can be observing the environment.
        unsafe {
            std::env::set_var("SteamAppId", APP_ID.to_string());
            std::env::set_var("SteamGameId", APP_ID.to_string());
        }

        // SAFETY: `path` is a real file under the user's Steam install; any
        // failure to load or to resolve a symbol is reported, never assumed.
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|err| SteamError::LibraryUnusable(err.to_string()))?;

        // SAFETY: each of these names was verified present in the client's
        // exported symbol table, and each signature is the documented flat-API
        // one. A missing symbol surfaces as `LibraryUnusable`.
        unsafe {
            let sym = |name: &[u8]| -> Result<*mut c_void, SteamError> {
                library.get::<*mut c_void>(name).map(|s| *s).map_err(|err| {
                    SteamError::LibraryUnusable(format!("{}: {err}", String::from_utf8_lossy(name)))
                })
            };

            let is_steam_running: unsafe extern "C" fn() -> bool =
                std::mem::transmute(sym(b"SteamAPI_IsSteamRunning\0")?);
            if !is_steam_running() {
                return Err(SteamError::NotRunning);
            }

            // `SteamAPI_InitFlat` writes a human-readable reason into a 1024-byte
            // buffer and returns 0 on success (ESteamAPIInitResult::OK).
            let init_flat: unsafe extern "C" fn(*mut c_char) -> c_int =
                std::mem::transmute(sym(b"SteamAPI_InitFlat\0")?);
            let mut err_msg = [0i8; 1024];
            let rc = init_flat(err_msg.as_mut_ptr());
            if rc != 0 {
                let why = CStr::from_ptr(err_msg.as_ptr())
                    .to_string_lossy()
                    .trim()
                    .to_owned();
                return Err(SteamError::InitFailed(if why.is_empty() {
                    format!("code {rc}")
                } else {
                    why
                }));
            }

            let shutdown: unsafe extern "C" fn() = std::mem::transmute(sym(b"SteamAPI_Shutdown\0")?);
            let steam_apps: unsafe extern "C" fn() -> *mut c_void =
                std::mem::transmute(sym(b"SteamAPI_SteamApps_v009\0")?);
            let get_app_install_dir: unsafe extern "C" fn(*mut c_void, u32, *mut c_char, u32) -> u32 =
                std::mem::transmute(sym(b"SteamAPI_ISteamApps_GetAppInstallDir\0")?);
            let is_app_installed: unsafe extern "C" fn(*mut c_void, u32) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamApps_BIsAppInstalled\0")?);
            let is_subscribed_app: unsafe extern "C" fn(*mut c_void, u32) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamApps_BIsSubscribedApp\0")?);

            let apps = steam_apps();
            if apps.is_null() {
                shutdown();
                return Err(SteamError::InitFailed(
                    "Steam returned no ISteamApps interface".to_owned(),
                ));
            }

            Ok(Self {
                shutdown,
                apps,
                get_app_install_dir,
                is_app_installed,
                is_subscribed_app,
                _library: library,
            })
        }
    }

    /// Whether the signed-in account owns the app.
    ///
    /// Init already required ownership, so this is a confirmation rather than a
    /// gate — worth reporting because family-shared and borrowed libraries are
    /// the cases users ask about.
    #[must_use]
    pub fn owns_app(&self) -> bool {
        // SAFETY: `apps` is the non-null interface pointer Steam returned, and
        // the call takes only plain integers.
        unsafe { (self.is_subscribed_app)(self.apps, APP_ID) }
    }

    /// Whether the app's files are installed locally.
    #[must_use]
    pub fn app_installed(&self) -> bool {
        // SAFETY: as `owns_app`.
        unsafe { (self.is_app_installed)(self.apps, APP_ID) }
    }

    /// The app's install directory.
    ///
    /// Steam answers this even for an app that is *not* installed (it reports
    /// where it would go), so callers must pair it with [`Self::app_installed`]
    /// before treating the path as real.
    #[must_use]
    pub fn app_install_dir(&self) -> Option<PathBuf> {
        let mut buf = [0i8; 4096];
        // SAFETY: the buffer and its length are handed over together, and Steam
        // writes at most `len` bytes including the terminator.
        let written = unsafe {
            (self.get_app_install_dir)(
                self.apps,
                APP_ID,
                buf.as_mut_ptr(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
            )
        };
        if written == 0 {
            return None;
        }
        // SAFETY: Steam nul-terminates within the buffer it was given.
        let path = unsafe { CStr::from_ptr(buf.as_ptr()) };
        let path = path.to_string_lossy();
        (!path.is_empty()).then(|| PathBuf::from(path.as_ref()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the connection is live until exactly here, and `shutdown`
        // runs before the library unloads (field order).
        unsafe { (self.shutdown)() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library is looked for under each root in order, and a root without
    /// one is skipped rather than failing the search.
    #[test]
    fn finds_the_library_under_a_modern_layout() {
        let root = std::env::temp_dir().join("kirie-steam-helper-find-test");
        let _ = std::fs::remove_dir_all(&root);
        let empty = root.join("old-client");
        let modern = root.join("new-client");
        std::fs::create_dir_all(empty.join("linux64")).expect("old layout");
        std::fs::create_dir_all(modern.join("steamrt64")).expect("new layout");
        std::fs::write(modern.join("steamrt64/libsteam_api.so"), b"not really a library").expect("library");

        let found = find_library(&[empty.clone(), modern.clone()]);
        assert_eq!(found, Some(modern.join("steamrt64/libsteam_api.so")));

        // A client too old to ship it is reported as absent, not scavenged for.
        assert_eq!(find_library(&[empty]), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}
