use std::ffi::{CString, c_char, c_double, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;

use gtk::prelude::ObjectType as _;

const SONAMES: [&str; 3] = [
    "libwebkit2gtk-4.1.so.0",
    "libwebkit2gtk-4.0.so.37",
    "libwebkit2gtk-4.0.so",
];

const INJECT_ALL_FRAMES: c_int = 0;

const INJECT_AT_DOCUMENT_START: c_int = 0;

type UserContentManagerNew = unsafe extern "C" fn() -> *mut c_void;

type UserContentManagerAddScript = unsafe extern "C" fn(*mut c_void, *mut c_void);

type UserScriptNew = unsafe extern "C" fn(
    *const c_char,
    c_int,
    c_int,
    *const *const c_char,
    *const *const c_char,
) -> *mut c_void;

type WebViewNewWithUserContentManager = unsafe extern "C" fn(*mut c_void) -> *mut gtk::ffi::GtkWidget;

type WebViewLoadUri = unsafe extern "C" fn(*mut c_void, *const c_char);

type WebViewSetBackgroundColor = unsafe extern "C" fn(*mut c_void, *const GdkRgba);

type WebViewGetSettings = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

type SettingsSetMediaPlaybackRequiresUserGesture = unsafe extern "C" fn(*mut c_void, c_int);

type WebContextGetDefault = unsafe extern "C" fn() -> *mut c_void;

type WebContextSetCacheModel = unsafe extern "C" fn(*mut c_void, c_int);

type SettingsSetEnablePageCache = unsafe extern "C" fn(*mut c_void, c_int);

type SettingsSetAllowFileAccess = unsafe extern "C" fn(*mut c_void, c_int);
type SettingsSetWriteConsoleToStdout = unsafe extern "C" fn(*mut c_void, c_int);

const CACHE_MODEL_DOCUMENT_VIEWER: c_int = 0;

type WebViewEvaluateJavascript = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    isize,
    *const c_char,
    *const c_char,
    *mut c_void,
    *mut c_void,
    *mut c_void,
);

type WebViewRunJavascript =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, *mut c_void, *mut c_void);

#[repr(C)]
struct GdkRgba {
    red: c_double,
    green: c_double,
    blue: c_double,
    alpha: c_double,
}

enum EvalFn {
    Evaluate(WebViewEvaluateJavascript),
    Run(WebViewRunJavascript),
}

pub struct WebKit {
    _lib: libloading::Library,
    soname: &'static str,
    user_content_manager_new: UserContentManagerNew,
    user_content_manager_add_script: UserContentManagerAddScript,
    user_script_new: UserScriptNew,
    web_view_new_with_user_content_manager: WebViewNewWithUserContentManager,
    web_view_load_uri: WebViewLoadUri,
    web_view_set_background_color: WebViewSetBackgroundColor,
    web_view_get_settings: WebViewGetSettings,
    settings_set_media_gesture: SettingsSetMediaPlaybackRequiresUserGesture,
    eval: EvalFn,
    web_context_get_default: Option<WebContextGetDefault>,
    web_context_set_cache_model: Option<WebContextSetCacheModel>,
    settings_set_enable_page_cache: Option<SettingsSetEnablePageCache>,
    settings_set_allow_file_access: Option<SettingsSetAllowFileAccess>,
    settings_set_write_console: Option<SettingsSetWriteConsoleToStdout>,
}

impl WebKit {
    pub fn load() -> Result<&'static Self, String> {
        static WEBKIT: OnceLock<Result<WebKit, String>> = OnceLock::new();
        WEBKIT
            .get_or_init(open_first_available)
            .as_ref()
            .map_err(Clone::clone)
    }

    #[must_use]
    pub fn soname(&self) -> &'static str {
        self.soname
    }

    pub fn minimize_caches(&self) {
        let (Some(get_default), Some(set_model)) =
            (self.web_context_get_default, self.web_context_set_cache_model)
        else {
            return;
        };
        // SAFETY: both aliases transcribe the WebKitGTK headers; the default
        unsafe {
            let ctx = get_default();
            if !ctx.is_null() {
                set_model(ctx, CACHE_MODEL_DOCUMENT_VIEWER);
            }
        }
    }

    pub fn new_web_view(&self, init_script: Option<&str>) -> *mut gtk::ffi::GtkWidget {
        // SAFETY: `webkit_user_content_manager_new` takes no arguments and
        let manager = unsafe { (self.user_content_manager_new)() };

        if let Some(source) = init_script {
            match CString::new(source) {
                Ok(source) => {
                    // SAFETY: `source` is a valid NUL-terminated C string that
                    let script = unsafe {
                        (self.user_script_new)(
                            source.as_ptr(),
                            INJECT_ALL_FRAMES,
                            INJECT_AT_DOCUMENT_START,
                            ptr::null(),
                            ptr::null(),
                        )
                    };
                    // SAFETY: both pointers were just produced by webkit's own
                    unsafe { (self.user_content_manager_add_script)(manager, script) };
                }
                Err(e) => tracing::warn!(error = %e, "init script contains a NUL; not injected"),
            }
        }

        // SAFETY: `manager` is a live `WebKitUserContentManager`; the web view
        unsafe { (self.web_view_new_with_user_content_manager)(manager) }
    }

    pub fn load_uri(&self, view: &gtk::Widget, uri: &str) {
        let Ok(uri) = CString::new(uri) else {
            tracing::error!("wallpaper url contains a NUL byte; not loading");
            return;
        };
        // SAFETY: see `as_web_view`; `uri` is a valid NUL-terminated C
        unsafe { (self.web_view_load_uri)(as_web_view(view), uri.as_ptr()) };
    }

    pub fn set_background_color(&self, view: &gtk::Widget, rgba: [f64; 4]) {
        let rgba = GdkRgba {
            red: rgba[0],
            green: rgba[1],
            blue: rgba[2],
            alpha: rgba[3],
        };
        // SAFETY: see `as_web_view`; `&rgba` is a valid `GdkRGBA` for the
        unsafe { (self.web_view_set_background_color)(as_web_view(view), &raw const rgba) };
    }

    pub fn set_autoplay(&self, view: &gtk::Widget, allow: bool) {
        // SAFETY: see `as_web_view`. `webkit_web_view_get_settings` returns a
        let settings = unsafe { (self.web_view_get_settings)(as_web_view(view)) };
        if settings.is_null() {
            tracing::warn!("webkit view has no settings object; autoplay left at its default");
            return;
        }
        // SAFETY: `settings` is a live `WebKitSettings` owned by `view`, which
        unsafe { (self.settings_set_media_gesture)(settings, c_int::from(!allow)) };

        if let Some(set_page_cache) = self.settings_set_enable_page_cache {
            // SAFETY: same live `WebKitSettings` as above; the value is a
            unsafe { set_page_cache(settings, 0) };
        }

        if let Some(set_file_access) = self.settings_set_allow_file_access {
            // SAFETY: same live `WebKitSettings`; plain gboolean.
            unsafe { set_file_access(settings, 1) };
        } else {
            tracing::warn!(
                "webkit lacks allow-file-access-from-file-urls; pages that XHR their own files will stay empty"
            );
        }
    }

    pub fn write_console_to_stdout(&self, view: &gtk::Widget) {
        let Some(set_write_console) = self.settings_set_write_console else {
            tracing::warn!("webkit lacks enable-write-console-messages-to-stdout; page console stays silent");
            return;
        };
        // SAFETY: see `as_web_view`; the settings object is owned by `view`.
        let settings = unsafe { (self.web_view_get_settings)(as_web_view(view)) };
        if settings.is_null() {
            return;
        }
        // SAFETY: same live `WebKitSettings`; plain gboolean.
        unsafe { set_write_console(settings, 1) };
    }

    pub fn eval(&self, view: &gtk::Widget, js: &str) {
        let Ok(js) = CString::new(js) else {
            tracing::debug!("script contains a NUL byte; not evaluated");
            return;
        };
        let view = as_web_view(view);
        match self.eval {
            // SAFETY: see `as_web_view`. `js` outlives the call and webkit
            EvalFn::Evaluate(f) => unsafe {
                f(
                    view,
                    js.as_ptr(),
                    -1,
                    ptr::null(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
            },
            // SAFETY: as above, minus the length/world/source arguments this
            EvalFn::Run(f) => unsafe {
                f(
                    view,
                    js.as_ptr(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
            },
        }
    }
}

fn as_web_view(view: &gtk::Widget) -> *mut c_void {
    view.as_ptr().cast::<c_void>()
}

fn open_first_available() -> Result<WebKit, String> {
    let mut rejected = Vec::new();
    for soname in SONAMES {
        // SAFETY: `dlopen` runs the library's initialisers, which is the
        let lib = match unsafe { libloading::Library::new(soname) } {
            Ok(lib) => lib,
            Err(e) => {
                rejected.push(format!("{soname}: {e}"));
                continue;
            }
        };
        match bind(lib, soname) {
            Ok(webkit) => {
                tracing::info!(
                    soname,
                    evaluate_javascript = matches!(webkit.eval, EvalFn::Evaluate(_)),
                    "loaded WebKitGTK via dlopen"
                );
                return Ok(webkit);
            }
            Err(e) => rejected.push(format!("{soname}: {e}")),
        }
    }
    Err(format!(
        "no usable WebKitGTK found — install webkit2gtk 4.1 or 4.0 (tried {})",
        rejected.join("; ")
    ))
}

fn bind(lib: libloading::Library, soname: &'static str) -> Result<WebKit, String> {
    // SAFETY: the aliases above transcribe the WebKitGTK headers verbatim, and
    let webkit = unsafe {
        WebKit {
            soname,
            user_content_manager_new: symbol(&lib, b"webkit_user_content_manager_new\0")?,
            user_content_manager_add_script: symbol(&lib, b"webkit_user_content_manager_add_script\0")?,
            user_script_new: symbol(&lib, b"webkit_user_script_new\0")?,
            web_view_new_with_user_content_manager: symbol(
                &lib,
                b"webkit_web_view_new_with_user_content_manager\0",
            )?,
            web_view_load_uri: symbol(&lib, b"webkit_web_view_load_uri\0")?,
            web_view_set_background_color: symbol(&lib, b"webkit_web_view_set_background_color\0")?,
            web_view_get_settings: symbol(&lib, b"webkit_web_view_get_settings\0")?,
            settings_set_media_gesture: symbol(
                &lib,
                b"webkit_settings_set_media_playback_requires_user_gesture\0",
            )?,
            eval: match symbol(&lib, b"webkit_web_view_evaluate_javascript\0") {
                Ok(f) => EvalFn::Evaluate(f),
                Err(_) => EvalFn::Run(symbol(&lib, b"webkit_web_view_run_javascript\0")?),
            },
            web_context_get_default: symbol(&lib, b"webkit_web_context_get_default\0").ok(),
            web_context_set_cache_model: symbol(&lib, b"webkit_web_context_set_cache_model\0").ok(),
            settings_set_enable_page_cache: symbol(&lib, b"webkit_settings_set_enable_page_cache\0").ok(),
            settings_set_allow_file_access: symbol(
                &lib,
                b"webkit_settings_set_allow_file_access_from_file_urls\0",
            )
            .ok(),
            settings_set_write_console: symbol(
                &lib,
                b"webkit_settings_set_enable_write_console_messages_to_stdout\0",
            )
            .ok(),
            _lib: lib,
        }
    };
    Ok(webkit)
}

unsafe fn symbol<T: Copy>(lib: &libloading::Library, name: &'static [u8]) -> Result<T, String> {
    // SAFETY: delegated to this function's own contract. `Library::get`
    let symbol: libloading::Symbol<'_, T> = unsafe { lib.get(name) }.map_err(|e| {
        let name = String::from_utf8_lossy(&name[..name.len() - 1]);
        format!("missing symbol {name} ({e})")
    })?;
    Ok(*symbol)
}
