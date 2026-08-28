use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};

pub const APP_ID: u32 = 431_960;

const LIB_RELATIVE: [&str; 2] = ["steamrt64/libsteam_api.so", "steamrt32/libsteam_api.so"];

#[derive(Debug)]
pub enum SteamError {
    LibraryMissing,
    LibraryUnusable(String),
    NotRunning,
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

#[must_use]
pub fn find_library(steam_roots: &[PathBuf]) -> Option<PathBuf> {
    steam_roots
        .iter()
        .flat_map(|root| LIB_RELATIVE.iter().map(move |rel| root.join(rel)))
        .find(|path| path.is_file())
}

pub struct Session {
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
    pub fn open(steam_roots: &[PathBuf]) -> Result<Self, SteamError> {
        let path = find_library(steam_roots).ok_or(SteamError::LibraryMissing)?;
        Self::open_library(&path)
    }

    pub fn open_library(path: &Path) -> Result<Self, SteamError> {
        // SAFETY: single-threaded at this point — `main` has spawned nothing,
        unsafe {
            std::env::set_var("SteamAppId", APP_ID.to_string());
            std::env::set_var("SteamGameId", APP_ID.to_string());
        }

        // SAFETY: `path` is a real file under the user's Steam install; any
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|err| SteamError::LibraryUnusable(err.to_string()))?;

        // SAFETY: each of these names was verified present in the client's
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

    #[must_use]
    pub fn owns_app(&self) -> bool {
        // SAFETY: `apps` is the non-null interface pointer Steam returned, and
        unsafe { (self.is_subscribed_app)(self.apps, APP_ID) }
    }

    #[must_use]
    pub fn app_installed(&self) -> bool {
        // SAFETY: as `owns_app`.
        unsafe { (self.is_app_installed)(self.apps, APP_ID) }
    }

    #[must_use]
    pub fn app_install_dir(&self) -> Option<PathBuf> {
        let mut buf = [0i8; 4096];
        // SAFETY: the buffer and its length are handed over together, and Steam
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
        unsafe { (self.shutdown)() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_eq!(find_library(&[empty]), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Sort {
    Popular,
    Trend,
    Recent,
    Rated,
}

impl Sort {
    const fn as_query(self) -> i32 {
        match self {
            Self::Rated => 0,
            Self::Recent => 1,
            Self::Trend => 3,
            Self::Popular => 12,
        }
    }

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

#[derive(Debug)]
pub struct Query {
    pub text: Option<String>,
    pub required_tags: Vec<String>,
    pub excluded_tags: Vec<String>,
    pub match_any_tag: bool,
    pub trend_days: Option<u32>,
    pub page: u32,
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

#[derive(Debug, Default, Clone, Copy)]
pub struct ItemState {
    pub subscribed: bool,
    pub installed: bool,
    pub needs_update: bool,
    pub downloading: bool,
    pub download_pending: bool,
}

impl ItemState {
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

#[derive(Debug)]
pub struct InstallInfo {
    pub size: u64,
    pub folder: PathBuf,
    pub updated: u32,
}

#[derive(Debug)]
pub struct DownloadInfo {
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Debug)]
pub struct Found {
    pub id: u64,
    pub title: String,
    pub owner: u64,
    pub created: u32,
    pub updated: u32,
    pub size: u32,
    pub votes_up: u32,
    pub votes_down: u32,
    pub score: f32,
    pub preview_url: Option<String>,
    pub tags: Vec<String>,
}

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

const MAX_TAGS: u32 = 64;

fn layout_plausible(d: &UgcDetails) -> bool {
    d.score.is_finite()
        && (0.0..=1.0).contains(&d.score)
        && d.votes_up < 100_000_000
        && d.votes_down < 100_000_000
}

fn c_field(bytes: &[c_char]) -> String {
    let len = bytes.iter().position(|c| *c == 0).unwrap_or(bytes.len());
    // SAFETY: `c_char` and `u8` have the same layout, and the slice is bounded
    let raw = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), len) };
    String::from_utf8_lossy(raw).into_owned()
}

impl Session {
    #[must_use]
    pub fn item_state(&self, id: u64) -> ItemState {
        // SAFETY: the interface pointer Steam returned, plus an integer.
        ItemState::from_bits(unsafe { (self.get_item_state)(self.ugc, id) })
    }

    #[must_use]
    pub fn item_install_info(&self, id: u64) -> Option<InstallInfo> {
        let mut size = 0u64;
        let mut folder = [0i8; 4096];
        let mut updated = 0u32;
        // SAFETY: every out-pointer is a live local, and the buffer's length is
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

    #[must_use]
    pub fn item_download_info(&self, id: u64) -> Option<DownloadInfo> {
        let mut downloaded = 0u64;
        let mut total = 0u64;
        // SAFETY: as `item_install_info`, with two out-pointers.
        let got = unsafe { (self.get_item_download_info)(self.ugc, id, &raw mut downloaded, &raw mut total) };
        got.then_some(DownloadInfo { downloaded, total })
    }

    pub fn subscribe(&self, id: u64, timeout: std::time::Duration) -> Result<(), SteamError> {
        // SAFETY: interface pointers Steam returned, plain integers, and one
        unsafe {
            let call = (self.subscribe_item)(self.ugc, id);
            if call == 0 {
                return Err(SteamError::InitFailed(
                    "Steam did not accept the subscription".to_owned(),
                ));
            }
            self.await_call(call, timeout)?;

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

            let state = ItemState::from_bits((self.get_item_state)(self.ugc, id));
            if !state.installed || state.needs_update {
                (self.download_item)(self.ugc, id, true);
            }
        }
        Ok(())
    }

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

    pub fn search(&self, query: &Query, timeout: std::time::Duration) -> Result<Vec<Found>, SteamError> {
        let ugc = self.ugc;
        let sort = query.sort.as_query();

        // SAFETY: every call below takes the interface pointer Steam handed us
        unsafe {
            let handle = (self.create_query)(ugc, sort, 0, APP_ID, APP_ID, query.page.max(1));
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
            (self.set_return_metadata)(ugc, handle, true);

            let call = (self.send_query)(ugc, handle);
            if call == 0 {
                release(handle);
                return Err(SteamError::InitFailed(
                    "Steam did not accept the query".to_owned(),
                ));
            }

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

                if !layout_plausible(&d) {
                    release(handle);
                    return Err(SteamError::InitFailed(
                        "Steam's SteamUGCDetails_t no longer matches the layout kirie reads; \
                         refusing to report made-up numbers"
                            .to_owned(),
                    ));
                }

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
