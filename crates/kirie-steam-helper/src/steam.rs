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

use std::ffi::{CStr, CString, c_char, c_int, c_void};
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
    ugc: *mut c_void,
    utils: *mut c_void,
    create_query: unsafe extern "C" fn(*mut c_void, i32, i32, u32, u32, u32) -> u64,
    set_search_text: unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> bool,
    add_required_tag: unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> bool,
    add_excluded_tag: unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> bool,
    set_match_any_tag: unsafe extern "C" fn(*mut c_void, u64, bool) -> bool,
    set_trend_days: unsafe extern "C" fn(*mut c_void, u64, u32) -> bool,
    set_return_metadata: unsafe extern "C" fn(*mut c_void, u64, bool) -> bool,
    send_query: unsafe extern "C" fn(*mut c_void, u64) -> u64,
    get_query_result: unsafe extern "C" fn(*mut c_void, u64, u32, *mut UgcDetails) -> bool,
    get_preview_url: unsafe extern "C" fn(*mut c_void, u64, u32, *mut c_char, u32) -> bool,
    get_num_tags: unsafe extern "C" fn(*mut c_void, u64, u32) -> u32,
    get_tag: unsafe extern "C" fn(*mut c_void, u64, u32, u32, *mut c_char, u32) -> bool,
    release_query: unsafe extern "C" fn(*mut c_void, u64) -> bool,
    subscribe_item: unsafe extern "C" fn(*mut c_void, u64) -> u64,
    unsubscribe_item: unsafe extern "C" fn(*mut c_void, u64) -> u64,
    download_item: unsafe extern "C" fn(*mut c_void, u64, bool) -> bool,
    get_item_state: unsafe extern "C" fn(*mut c_void, u64) -> u32,
    get_item_install_info:
        unsafe extern "C" fn(*mut c_void, u64, *mut u64, *mut c_char, u32, *mut u32) -> bool,
    get_item_download_info: unsafe extern "C" fn(*mut c_void, u64, *mut u64, *mut u64) -> bool,
    is_call_completed: unsafe extern "C" fn(*mut c_void, u64, *mut bool) -> bool,
    get_call_result: unsafe extern "C" fn(*mut c_void, u64, *mut c_void, i32, i32, *mut bool) -> bool,
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

            let steam_ugc: unsafe extern "C" fn() -> *mut c_void =
                std::mem::transmute(sym(b"SteamAPI_SteamUGC_v021\0")?);
            let steam_utils: unsafe extern "C" fn() -> *mut c_void =
                std::mem::transmute(sym(b"SteamAPI_SteamUtils_v011\0")?);
            let create_query: unsafe extern "C" fn(*mut c_void, i32, i32, u32, u32, u32) -> u64 =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_CreateQueryAllUGCRequestPage\0")?);
            let set_search_text: unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_SetSearchText\0")?);
            let add_required_tag: unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_AddRequiredTag\0")?);
            let add_excluded_tag: unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_AddExcludedTag\0")?);
            let set_match_any_tag: unsafe extern "C" fn(*mut c_void, u64, bool) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_SetMatchAnyTag\0")?);
            let set_trend_days: unsafe extern "C" fn(*mut c_void, u64, u32) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_SetRankedByTrendDays\0")?);
            let set_return_metadata: unsafe extern "C" fn(*mut c_void, u64, bool) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_SetReturnMetadata\0")?);
            let send_query: unsafe extern "C" fn(*mut c_void, u64) -> u64 =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_SendQueryUGCRequest\0")?);
            let get_query_result: unsafe extern "C" fn(*mut c_void, u64, u32, *mut UgcDetails) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetQueryUGCResult\0")?);
            let get_preview_url: unsafe extern "C" fn(*mut c_void, u64, u32, *mut c_char, u32) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetQueryUGCPreviewURL\0")?);
            // Tags come from these accessors, not from `SteamUGCDetails_t`:
            // measured against the live client, `m_rgchTags` stays empty even
            // with `SetReturnMetadata` on, while the accessors answer.
            let get_num_tags: unsafe extern "C" fn(*mut c_void, u64, u32) -> u32 =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetQueryUGCNumTags\0")?);
            let get_tag: unsafe extern "C" fn(*mut c_void, u64, u32, u32, *mut c_char, u32) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetQueryUGCTag\0")?);
            let release_query: unsafe extern "C" fn(*mut c_void, u64) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_ReleaseQueryUGCRequest\0")?);
            let subscribe_item: unsafe extern "C" fn(*mut c_void, u64) -> u64 =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_SubscribeItem\0")?);
            let unsubscribe_item: unsafe extern "C" fn(*mut c_void, u64) -> u64 =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_UnsubscribeItem\0")?);
            let download_item: unsafe extern "C" fn(*mut c_void, u64, bool) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_DownloadItem\0")?);
            let get_item_state: unsafe extern "C" fn(*mut c_void, u64) -> u32 =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetItemState\0")?);
            let get_item_install_info: unsafe extern "C" fn(
                *mut c_void,
                u64,
                *mut u64,
                *mut c_char,
                u32,
                *mut u32,
            ) -> bool = std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetItemInstallInfo\0")?);
            let get_item_download_info: unsafe extern "C" fn(*mut c_void, u64, *mut u64, *mut u64) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUGC_GetItemDownloadInfo\0")?);
            let is_call_completed: unsafe extern "C" fn(*mut c_void, u64, *mut bool) -> bool =
                std::mem::transmute(sym(b"SteamAPI_ISteamUtils_IsAPICallCompleted\0")?);
            let get_call_result: unsafe extern "C" fn(
                *mut c_void,
                u64,
                *mut c_void,
                i32,
                i32,
                *mut bool,
            ) -> bool = std::mem::transmute(sym(b"SteamAPI_ISteamUtils_GetAPICallResult\0")?);

            let ugc = steam_ugc();
            let utils = steam_utils();
            let apps = steam_apps();
            if ugc.is_null() || utils.is_null() {
                shutdown();
                return Err(SteamError::InitFailed(
                    "Steam returned no ISteamUGC/ISteamUtils interface".to_owned(),
                ));
            }
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
                ugc,
                utils,
                create_query,
                set_search_text,
                add_required_tag,
                add_excluded_tag,
                set_match_any_tag,
                set_trend_days,
                set_return_metadata,
                send_query,
                get_query_result,
                get_preview_url,
                get_num_tags,
                get_tag,
                release_query,
                subscribe_item,
                unsubscribe_item,
                download_item,
                get_item_state,
                get_item_install_info,
                get_item_download_info,
                is_call_completed,
                get_call_result,
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

/// How results are ordered. Values are Steam's `EUGCQuery`.
///
/// Deliberately a small named set rather than the full enum: these are the
/// orderings a person browsing wallpapers actually asks for. **Do not** reuse
/// the Web API's `EPublishedFileQueryType` numbers — they disagree with these
/// for the same names.
#[derive(Debug, Clone, Copy)]
pub enum Sort {
    /// Most subscribed overall.
    Popular,
    /// Trending within a recent window (see `trend_days`).
    Trend,
    /// Most recently published.
    Recent,
    /// Highest rated.
    Rated,
}

impl Sort {
    const fn as_query(self) -> i32 {
        match self {
            Self::Rated => 0,    // RankedByVote
            Self::Recent => 1,   // RankedByPublicationDate
            Self::Trend => 3,    // RankedByTrend
            Self::Popular => 12, // RankedByTotalUniqueSubscriptions
        }
    }

    /// Parse the CLI spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "popular" | "subs" => Some(Self::Popular),
            "trend" | "trending" => Some(Self::Trend),
            "recent" | "new" => Some(Self::Recent),
            "rated" | "votes" => Some(Self::Rated),
            _ => None,
        }
    }
}

/// What to ask the Workshop for.
#[derive(Debug)]
pub struct Query {
    /// Free-text search.
    pub text: Option<String>,
    /// Tags an item must carry (all of them, unless `match_any_tag`).
    pub required_tags: Vec<String>,
    /// Tags that exclude an item.
    pub excluded_tags: Vec<String>,
    /// Treat `required_tags` as "any of" rather than "all of".
    pub match_any_tag: bool,
    /// Window for [`Sort::Trend`], in days.
    pub trend_days: Option<u32>,
    /// 1-based page; Steam returns up to 50 per page.
    pub page: u32,
    /// Result ordering.
    pub sort: Sort,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            text: None,
            required_tags: Vec::new(),
            excluded_tags: Vec::new(),
            match_any_tag: false,
            trend_days: None,
            page: 1,
            sort: Sort::Popular,
        }
    }
}

/// What Steam knows about one item right now.
///
/// The bits are `EItemStateFlags`; they are decoded here rather than passed
/// through as a number so a caller never has to know Valve's constants.
#[derive(Debug, Default, Clone, Copy)]
pub struct ItemState {
    /// This account is subscribed to the item.
    pub subscribed: bool,
    /// Its files are on disk.
    pub installed: bool,
    /// Installed, but a newer version exists.
    pub needs_update: bool,
    /// Steam is downloading it now.
    pub downloading: bool,
    /// Queued to download.
    pub download_pending: bool,
}

impl ItemState {
    /// Decode an `EItemStateFlags` bitfield.
    fn from_bits(bits: u32) -> Self {
        Self {
            subscribed: bits & 1 != 0,
            installed: bits & 4 != 0,
            needs_update: bits & 8 != 0,
            downloading: bits & 16 != 0,
            download_pending: bits & 32 != 0,
        }
    }
}

/// Where an installed item's files are.
#[derive(Debug)]
pub struct InstallInfo {
    /// Bytes on disk.
    pub size: u64,
    /// The directory holding `project.json`.
    pub folder: PathBuf,
    /// When Steam last wrote it, as a Unix timestamp.
    pub updated: u32,
}

/// Download progress, as bytes fetched of the total.
#[derive(Debug)]
pub struct DownloadInfo {
    /// Bytes fetched so far.
    pub downloaded: u64,
    /// Total bytes, or 0 while Steam has not yet decided.
    pub total: u64,
}

/// One Workshop item as the query returned it.
#[derive(Debug)]
pub struct Found {
    /// Workshop id.
    pub id: u64,
    /// Item title.
    pub title: String,
    /// Author's SteamID64 (resolving it to a name needs a separate call).
    pub owner: u64,
    /// Publish and update times, unix seconds.
    pub created: u32,
    /// Last update, unix seconds.
    pub updated: u32,
    /// Payload size in bytes.
    pub size: u32,
    /// Up/down votes and Steam's own score.
    pub votes_up: u32,
    /// Down votes.
    pub votes_down: u32,
    /// Steam's score, 0..1.
    pub score: f32,
    /// The preview image, when the item has one.
    pub preview_url: Option<String>,
    /// Tags, as Steam spells them.
    pub tags: Vec<String>,
}

/// `SteamUGCDetails_t`, laid out exactly as the SDK declares it.
///
/// Every field is declared because the ones kirie reads only land correctly if
/// everything before them is the right size. This was verified against the
/// live client rather than trusted: an early version had `title` one byte too
/// long, which shifted `votes_up` onto `score` and reported a billion votes.
/// [`Session::search`] asserts the two vote fields and the score still line up,
/// so a future SDK that moves them fails loudly instead of reporting nonsense.
#[repr(C)]
#[derive(Clone, Copy)]
struct UgcDetails {
    published_file_id: u64,
    result: i32,
    file_type: i32,
    creator_app_id: u32,
    consumer_app_id: u32,
    title: [c_char; 128],
    description: [c_char; 8000],
    steam_id_owner: u64,
    time_created: u32,
    time_updated: u32,
    time_added_to_user_list: u32,
    visibility: i32,
    banned: bool,
    accepted_for_use: bool,
    tags_truncated: bool,
    tags: [c_char; 1025],
    file: u64,
    preview_file: u64,
    filename: [c_char; 260],
    file_size: i32,
    preview_file_size: i32,
    url: [c_char; 256],
    votes_up: u32,
    votes_down: u32,
    score: f32,
    num_children: u32,
    total_files_size: u64,
}

/// Most tags a single item can report before the list is treated as garbage.
///
/// Wallpaper Engine items carry a handful; the cap exists so a wrong count from
/// a changed SDK cannot drive an unbounded allocation (V9).
const MAX_TAGS: u32 = 64;

/// Whether a result's numbers are shaped like the fields they claim to be.
///
/// The only defence against a silently reshuffled `SteamUGCDetails_t`: every
/// field would still parse, just from the wrong bytes.
fn layout_plausible(d: &UgcDetails) -> bool {
    d.score.is_finite()
        && (0.0..=1.0).contains(&d.score)
        // Steam's most-subscribed item has ~10 million votes; a field read from
        // the wrong offset lands orders of magnitude past that.
        && d.votes_up < 100_000_000
        && d.votes_down < 100_000_000
}

/// Read a fixed-size C string field without running off its end.
fn c_field(bytes: &[c_char]) -> String {
    let len = bytes.iter().position(|c| *c == 0).unwrap_or(bytes.len());
    // SAFETY: `c_char` and `u8` have the same layout, and the slice is bounded
    // by the nul we just located (or the field's own length).
    let raw = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), len) };
    String::from_utf8_lossy(raw).into_owned()
}

impl Session {
    /// What Steam currently knows about an item.
    #[must_use]
    pub fn item_state(&self, id: u64) -> ItemState {
        // SAFETY: the interface pointer Steam returned, plus an integer.
        ItemState::from_bits(unsafe { (self.get_item_state)(self.ugc, id) })
    }

    /// Where an item's files are, if Steam has them.
    #[must_use]
    pub fn item_install_info(&self, id: u64) -> Option<InstallInfo> {
        let mut size = 0u64;
        let mut folder = [0i8; 4096];
        let mut updated = 0u32;
        // SAFETY: every out-pointer is a live local, and the buffer's length is
        // handed over with it so Steam writes at most that many bytes.
        let got = unsafe {
            (self.get_item_install_info)(
                self.ugc,
                id,
                &raw mut size,
                folder.as_mut_ptr(),
                u32::try_from(folder.len()).unwrap_or(u32::MAX),
                &raw mut updated,
            )
        };
        if !got {
            return None;
        }
        let folder = c_field(&folder);
        (!folder.is_empty()).then(|| InstallInfo {
            size,
            folder: PathBuf::from(folder),
            updated,
        })
    }

    /// Download progress for an item Steam is fetching.
    ///
    /// `None` when Steam is not downloading it — which, for a caller waiting on
    /// a subscription, means either "not started yet" or "already done", so it
    /// must be read together with [`Self::item_state`].
    #[must_use]
    pub fn item_download_info(&self, id: u64) -> Option<DownloadInfo> {
        let mut downloaded = 0u64;
        let mut total = 0u64;
        // SAFETY: as `item_install_info`, with two out-pointers.
        let got = unsafe { (self.get_item_download_info)(self.ugc, id, &raw mut downloaded, &raw mut total) };
        got.then_some(DownloadInfo { downloaded, total })
    }

    /// Subscribe this account to an item, and ask Steam to fetch it.
    ///
    /// Returns once Steam has *accepted the subscription*, not once the files
    /// have landed. Waiting for the download is deliberately not done here: a
    /// held session is a "playing Wallpaper Engine" presence and accrues
    /// playtime (see the module docs), so the caller watches the filesystem
    /// instead, with this process already gone.
    pub fn subscribe(&self, id: u64, timeout: std::time::Duration) -> Result<(), SteamError> {
        // SAFETY: interface pointers Steam returned, plain integers, and one
        // out-parameter that is a live local for the length of each call.
        unsafe {
            let call = (self.subscribe_item)(self.ugc, id);
            if call == 0 {
                return Err(SteamError::InitFailed(
                    "Steam did not accept the subscription".to_owned(),
                ));
            }
            self.await_call(call, timeout)?;

            // `RemoteStorageSubscribePublishedFileResult_t` is callback id 1313
            // (k_iSteamRemoteStorageCallbacks + 13).
            #[repr(C)]
            struct SubscribeResult {
                result: i32,
                published_file_id: u64,
            }
            let mut done = std::mem::MaybeUninit::<SubscribeResult>::zeroed();
            let mut failed = false;
            let got = (self.get_call_result)(
                self.utils,
                call,
                done.as_mut_ptr().cast(),
                i32::try_from(std::mem::size_of::<SubscribeResult>()).unwrap_or(i32::MAX),
                1313,
                &raw mut failed,
            );
            if !got || failed {
                return Err(SteamError::InitFailed(
                    "Steam returned no subscription result".to_owned(),
                ));
            }
            let done = done.assume_init();
            if done.result != 1 {
                return Err(SteamError::InitFailed(format!(
                    "Steam refused the subscription with result {}",
                    done.result
                )));
            }

            // Subscribing normally queues the download on its own; asking
            // explicitly makes it start now rather than at the client's next
            // sweep. Only when there is something to fetch, though: asking for
            // an item already on disk and up to date makes Steam queue a
            // pointless re-validation (measured: `download_pending` flipped
            // true for an item that was already installed).
            let state = ItemState::from_bits((self.get_item_state)(self.ugc, id));
            if !state.installed || state.needs_update {
                (self.download_item)(self.ugc, id, true);
            }
        }
        Ok(())
    }

    /// Drop this account's subscription to an item.
    ///
    /// Steam removes the files itself, on its own schedule — this reports the
    /// item's state once it has accepted, which is the honest answer: the
    /// directory may still be on disk for a while.
    ///
    /// Unlike [`Self::subscribe`], the call result is not read back: the
    /// completion is enough, and the item's own state says what happened
    /// without depending on a callback id staying put across SDK versions.
    pub fn unsubscribe(&self, id: u64, timeout: std::time::Duration) -> Result<ItemState, SteamError> {
        // SAFETY: the interface pointer Steam returned plus a plain integer.
        let call = unsafe { (self.unsubscribe_item)(self.ugc, id) };
        if call == 0 {
            return Err(SteamError::InitFailed(
                "Steam did not accept the unsubscribe".to_owned(),
            ));
        }
        // SAFETY: `call` is the handle Steam just returned.
        unsafe { self.await_call(call, timeout) }?;
        Ok(self.item_state(id))
    }

    /// Block until an async call completes, or `timeout` elapses.
    ///
    /// # Safety
    ///
    /// `call` must be a handle this session's Steam API returned.
    unsafe fn await_call(&self, call: u64, timeout: std::time::Duration) -> Result<(), SteamError> {
        let started = std::time::Instant::now();
        loop {
            let mut failed = false;
            // SAFETY: the caller's contract, plus a live out-parameter.
            if unsafe { (self.is_call_completed)(self.utils, call, &raw mut failed) } {
                if failed {
                    return Err(SteamError::InitFailed("the Steam call failed".to_owned()));
                }
                return Ok(());
            }
            if started.elapsed() > timeout {
                return Err(SteamError::InitFailed("the Steam call timed out".to_owned()));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Run one Workshop query and return its page of results.
    ///
    /// Blocks until Steam answers or `timeout` elapses. The call is polled
    /// through `ISteamUtils` rather than the SDK's C++ callback machinery,
    /// which cannot be registered from Rust without vtable games.
    pub fn search(&self, query: &Query, timeout: std::time::Duration) -> Result<Vec<Found>, SteamError> {
        let ugc = self.ugc;
        let sort = query.sort.as_query();

        // SAFETY: every call below takes the interface pointer Steam handed us
        // plus plain integers or nul-terminated strings we own for the duration
        // of the call. The handle is released on every exit path.
        unsafe {
            let handle = (self.create_query)(
                ugc,
                sort,
                // k_EUGCMatchingUGCType_Items — wallpapers, not collections.
                0,
                APP_ID,
                APP_ID,
                query.page.max(1),
            );
            if handle == 0 || handle == u64::MAX {
                return Err(SteamError::InitFailed("Steam refused the query".to_owned()));
            }

            let release = |h: u64| (self.release_query)(ugc, h);

            if let Some(text) = &query.text
                && !text.trim().is_empty()
                && let Ok(c) = CString::new(text.as_str())
            {
                (self.set_search_text)(ugc, handle, c.as_ptr());
            }
            for tag in &query.required_tags {
                if let Ok(c) = CString::new(tag.as_str()) {
                    (self.add_required_tag)(ugc, handle, c.as_ptr());
                }
            }
            for tag in &query.excluded_tags {
                if let Ok(c) = CString::new(tag.as_str()) {
                    (self.add_excluded_tag)(ugc, handle, c.as_ptr());
                }
            }
            if query.match_any_tag {
                (self.set_match_any_tag)(ugc, handle, true);
            }
            if let Some(days) = query.trend_days {
                (self.set_trend_days)(ugc, handle, days);
            }
            // Without this Steam returns every item with an empty tag list.
            (self.set_return_metadata)(ugc, handle, true);

            let call = (self.send_query)(ugc, handle);
            if call == 0 {
                release(handle);
                return Err(SteamError::InitFailed(
                    "Steam did not accept the query".to_owned(),
                ));
            }

            // Poll rather than pump callbacks: this process exists for exactly
            // one request and then dies.
            let started = std::time::Instant::now();
            loop {
                let mut failed = false;
                if (self.is_call_completed)(self.utils, call, &raw mut failed) {
                    if failed {
                        release(handle);
                        return Err(SteamError::InitFailed("the Workshop query failed".to_owned()));
                    }
                    break;
                }
                if started.elapsed() > timeout {
                    release(handle);
                    return Err(SteamError::InitFailed("the Workshop query timed out".to_owned()));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }

            // `SteamUGCQueryCompleted_t` is callback id 3401 (k_iSteamUGCCallbacks + 1)
            // and reports how many results the page holds.
            #[repr(C)]
            struct QueryCompleted {
                handle: u64,
                result: i32,
                num_results_returned: u32,
                total_matching_results: u32,
                cached_data: bool,
                next_cursor: [c_char; 256],
            }
            let mut completed = std::mem::MaybeUninit::<QueryCompleted>::zeroed();
            let mut failed = false;
            let got = (self.get_call_result)(
                self.utils,
                call,
                completed.as_mut_ptr().cast(),
                i32::try_from(std::mem::size_of::<QueryCompleted>()).unwrap_or(i32::MAX),
                3401,
                &raw mut failed,
            );
            if !got || failed {
                release(handle);
                return Err(SteamError::InitFailed(
                    "Steam returned no query result".to_owned(),
                ));
            }
            let completed = completed.assume_init();
            if completed.result != 1 {
                release(handle);
                return Err(SteamError::InitFailed(format!(
                    "the Workshop query returned Steam result {}",
                    completed.result
                )));
            }

            // Bound the count by what Steam itself documents per page (V9: a
            // hostile or wrong count must not drive a huge allocation).
            let count = completed.num_results_returned.min(50);
            let mut found = Vec::with_capacity(count as usize);
            for index in 0..count {
                let mut details = std::mem::MaybeUninit::<UgcDetails>::zeroed();
                if !(self.get_query_result)(ugc, handle, index, details.as_mut_ptr()) {
                    continue;
                }
                let d = details.assume_init();

                let mut url = [0i8; 1024];
                let has_preview = (self.get_preview_url)(
                    ugc,
                    handle,
                    index,
                    url.as_mut_ptr(),
                    u32::try_from(url.len()).unwrap_or(u32::MAX),
                );
                let preview_url = has_preview
                    .then(|| CStr::from_ptr(url.as_ptr()).to_string_lossy().into_owned())
                    .filter(|u| !u.is_empty());

                // The struct is only trustworthy while the SDK's layout is
                // unchanged, and a shift reports nonsense rather than failing:
                // a `title` one byte too long once made `votes_up` read 1.06e9
                // (the bit pattern of the score float). A score is a fraction
                // and votes are counts, so check them and refuse loudly.
                if !layout_plausible(&d) {
                    release(handle);
                    return Err(SteamError::InitFailed(
                        "Steam's SteamUGCDetails_t no longer matches the layout kirie reads; \
                         refusing to report made-up numbers"
                            .to_owned(),
                    ));
                }

                // Tags come from the accessors: `m_rgchTags` comes back empty
                // from the live client even with `SetReturnMetadata` on. The
                // count is Steam's, so bound it (V9) before allocating.
                let num_tags = (self.get_num_tags)(ugc, handle, index).min(MAX_TAGS);
                let mut tags: Vec<String> = Vec::with_capacity(num_tags as usize);
                for tag_index in 0..num_tags {
                    let mut buf = [0i8; 256];
                    let got = (self.get_tag)(
                        ugc,
                        handle,
                        index,
                        tag_index,
                        buf.as_mut_ptr(),
                        u32::try_from(buf.len()).unwrap_or(u32::MAX),
                    );
                    if got {
                        let tag = c_field(&buf);
                        if !tag.is_empty() {
                            tags.push(tag);
                        }
                    }
                }
                if tags.is_empty() {
                    // A client old enough to fill the struct field instead.
                    tags.extend(
                        c_field(&d.tags)
                            .split(',')
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                            .map(ToOwned::to_owned),
                    );
                }

                found.push(Found {
                    id: d.published_file_id,
                    title: c_field(&d.title),
                    owner: d.steam_id_owner,
                    created: d.time_created,
                    updated: d.time_updated,
                    size: u32::try_from(d.file_size.max(0)).unwrap_or(0),
                    votes_up: d.votes_up,
                    votes_down: d.votes_down,
                    score: d.score,
                    preview_url,
                    tags,
                });
            }

            release(handle);
            Ok(found)
        }
    }
}
