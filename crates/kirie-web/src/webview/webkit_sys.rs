//! Runtime (`dlopen`) bindings to the WebKitGTK C API.
//!
//! # Why webkit is not linked at compile time
//!
//! `wry` 0.55 reaches webkit through `webkit2gtk-sys`, whose `system-deps`
//! manifest hard-requires the **`webkit2gtk-4.1`** pkg-config module. Every
//! binary built from it therefore carries a `DT_NEEDED` on
//! `libwebkit2gtk-4.1.so.0`, and a `kirie-webviewhost` produced on a current
//! distro cannot even reach `main` on the LTS releases that still ship only
//! `webkit2gtk-4.0` (Ubuntu 20.04, Debian 11) — the dynamic loader fails first.
//!
//! 4.0 and 4.1 are the *same* engine behind the *same* GTK3 C API; the ABI
//! split exists solely because 4.1 links libsoup-3 where 4.0 links libsoup-2.4
//! (upstream calls the pair "API version" 4.0/4.1 for exactly that reason).
//! This host never touches libsoup — no custom URI schemes, no
//! `WebKitWebContext` network configuration, no cookie/security-origin
//! plumbing — so every entry point below is identical, by name and by
//! signature, in both. Resolving them with `dlopen` at run time collapses two
//! incompatible build targets into one binary that runs on both, and as a
//! side effect lets the host be *built* on a machine with no webkit
//! development package at all.
//!
//! # Scope
//!
//! Deliberately tiny: eight required entry points plus one either/or pair for
//! running JavaScript. Anything that would pull webkit's `WebKitWebContext` /
//! `WebKitNetworkSession` surface is off-limits, because *that* is where the
//! 4.0/4.1 split actually bites.

use std::ffi::{CString, c_char, c_double, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;

use gtk::prelude::ObjectType as _;

/// Sonames tried, in order, when opening WebKitGTK.
///
/// The versioned sonames come first because they are what an end-user runtime
/// package installs; the bare `.so` link only exists alongside a `-dev`
/// package and is the last resort.
const SONAMES: [&str; 3] = [
    // Current distros (libsoup-3). Preferred: it is the build upstream still
    // maintains and the one that receives security updates.
    "libwebkit2gtk-4.1.so.0",
    // Old LTS (libsoup-2.4). `.37` is the ABI revision the whole WebKitGTK
    // 4.0 series carried (Ubuntu 20.04/22.04, Debian 11).
    "libwebkit2gtk-4.0.so.37",
    "libwebkit2gtk-4.0.so",
];

/// `WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES` — inject into sub-frames too, so an
/// `<iframe>`-based wallpaper still sees the WE bridge.
const INJECT_ALL_FRAMES: c_int = 0;

/// `WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START` — run before the page's own
/// scripts, which is the whole point of the bridge shim.
const INJECT_AT_DOCUMENT_START: c_int = 0;

// The bound C signatures. Each alias is spelled out rather than inferred so a
// mismatch is a review-visible diff against the WebKitGTK headers; `gpointer`
// and every opaque `WebKit*` instance are `*mut c_void` because this module
// only ever passes them straight back into webkit.

/// `WebKitUserContentManager *webkit_user_content_manager_new (void)`
type UserContentManagerNew = unsafe extern "C" fn() -> *mut c_void;

/// `void webkit_user_content_manager_add_script (WebKitUserContentManager *,
/// WebKitUserScript *)`
type UserContentManagerAddScript = unsafe extern "C" fn(*mut c_void, *mut c_void);

/// `WebKitUserScript *webkit_user_script_new (const gchar *source,
/// WebKitUserContentInjectedFrames, WebKitUserScriptInjectionTime,
/// const gchar * const *allow_list, const gchar * const *block_list)`
type UserScriptNew = unsafe extern "C" fn(
    *const c_char,
    c_int,
    c_int,
    *const *const c_char,
    *const *const c_char,
) -> *mut c_void;

/// `GtkWidget *webkit_web_view_new_with_user_content_manager
/// (WebKitUserContentManager *)`
type WebViewNewWithUserContentManager = unsafe extern "C" fn(*mut c_void) -> *mut gtk::ffi::GtkWidget;

/// `void webkit_web_view_load_uri (WebKitWebView *, const gchar *uri)`
type WebViewLoadUri = unsafe extern "C" fn(*mut c_void, *const c_char);

/// `void webkit_web_view_set_background_color (WebKitWebView *, const GdkRGBA *)`
type WebViewSetBackgroundColor = unsafe extern "C" fn(*mut c_void, *const GdkRgba);

/// `WebKitSettings *webkit_web_view_get_settings (WebKitWebView *)`
type WebViewGetSettings = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

/// `void webkit_settings_set_media_playback_requires_user_gesture
/// (WebKitSettings *, gboolean enabled)`
///
/// Note the spelling: unlike most `WebKitSettings` booleans this one has no
/// `enable_` in the setter (the property is `media-playback-requires-user-
/// gesture`, not `enable-…`), so `webkit_settings_set_enable_media_playback_
/// requires_user_gesture` does not exist in any WebKitGTK.
type SettingsSetMediaPlaybackRequiresUserGesture = unsafe extern "C" fn(*mut c_void, c_int);

/// `WebKitWebContext *webkit_web_context_get_default (void)`
type WebContextGetDefault = unsafe extern "C" fn() -> *mut c_void;

/// `void webkit_web_context_set_cache_model (WebKitWebContext *,
/// WebKitCacheModel)`
type WebContextSetCacheModel = unsafe extern "C" fn(*mut c_void, c_int);

/// `void webkit_settings_set_enable_page_cache (WebKitSettings *, gboolean)`
type SettingsSetEnablePageCache = unsafe extern "C" fn(*mut c_void, c_int);

/// `void webkit_settings_set_allow_file_access_from_file_urls
/// (WebKitSettings *, gboolean)` — webkit 2.10+.
type SettingsSetAllowFileAccess = unsafe extern "C" fn(*mut c_void, c_int);

/// `WEBKIT_CACHE_MODEL_DOCUMENT_VIEWER` — the smallest of the three cache
/// models: webkit keeps no back/forward page cache and only a minimal resource
/// cache. The default is `WEB_BROWSER`, which trades memory for revisit speed —
/// exactly the wrong trade for a wallpaper, which loads one document once and
/// never navigates.
const CACHE_MODEL_DOCUMENT_VIEWER: c_int = 0;

/// `void webkit_web_view_evaluate_javascript (WebKitWebView *, const char
/// *script, gssize length, const char *world_name, const char *source_uri,
/// GCancellable *, GAsyncReadyCallback, gpointer user_data)` — WebKitGTK 2.40+.
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

/// `void webkit_web_view_run_javascript (WebKitWebView *, const gchar *script,
/// GCancellable *, GAsyncReadyCallback, gpointer user_data)` — the pre-2.40
/// spelling, deprecated but still present on the old 4.0 builds.
type WebViewRunJavascript =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, *mut c_void, *mut c_void);

/// `GdkRGBA` — four normalised (0.0–1.0) `gdouble` channels.
///
/// Redeclared here instead of reusing `gdk::ffi::GdkRGBA` only to keep the
/// binding self-contained; the layout is fixed by the GDK3 ABI.
#[repr(C)]
struct GdkRgba {
    red: c_double,
    green: c_double,
    blue: c_double,
    alpha: c_double,
}

/// Which JavaScript entry point the loaded WebKitGTK offers.
///
/// `webkit_web_view_evaluate_javascript` landed in WebKitGTK 2.40 (2023) and
/// deprecated `webkit_web_view_run_javascript`; the 2.3x builds behind the old
/// `webkit2gtk-4.0` packages only have the latter. The two differ in more than
/// a name — `evaluate_javascript` takes an explicit script length plus a world
/// name and source URI for the resulting stack traces — so they cannot share
/// one alias and the choice is made once, at load time.
enum EvalFn {
    /// WebKitGTK >= 2.40.
    Evaluate(WebViewEvaluateJavascript),
    /// WebKitGTK < 2.40.
    Run(WebViewRunJavascript),
}

/// The process-wide, `dlopen`ed WebKitGTK entry points.
///
/// Obtained once via [`WebKit::load`]; the underlying [`libloading::Library`]
/// is owned by this struct and this struct only ever lives inside a `static`,
/// so webkit is never unloaded while a web view built from it is alive.
pub struct WebKit {
    /// Owns the `dlopen` handle. Never dropped (see [`WebKit::load`]): closing
    /// it would `dlclose` webkit out from under a live GTK widget.
    _lib: libloading::Library,
    /// Which of [`SONAMES`] actually loaded, for diagnostics.
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
    /// Memory-trimming entry points, resolved best-effort. Unlike the set
    /// above, a miss here must NOT reject the library: these only shrink the
    /// footprint, so an older webkit that lacks one still renders correctly —
    /// it just keeps its default caches.
    web_context_get_default: Option<WebContextGetDefault>,
    web_context_set_cache_model: Option<WebContextSetCacheModel>,
    settings_set_enable_page_cache: Option<SettingsSetEnablePageCache>,
    /// `file://` pages XHR/fetching sibling `file://` resources (data.json,
    /// packaged media) — denied by webkit's default same-origin rule, allowed
    /// by the reference engine (CEF `allow-file-access-from-files`).
    settings_set_allow_file_access: Option<SettingsSetAllowFileAccess>,
}

impl WebKit {
    /// Open WebKitGTK and resolve every entry point, once per process.
    ///
    /// The result — success *or* failure — is cached in a `OnceLock`, so a
    /// system without webkit is not re-probed on every call and the successful
    /// `Library` is kept alive for the process lifetime (a `static` is never
    /// dropped, which is exactly the guarantee the returned function pointers
    /// need).
    ///
    /// # Errors
    ///
    /// Returns a human-readable summary listing every soname tried and why it
    /// was rejected — either `dlopen` failed or a required symbol was missing.
    pub fn load() -> Result<&'static Self, String> {
        static WEBKIT: OnceLock<Result<WebKit, String>> = OnceLock::new();
        WEBKIT
            .get_or_init(open_first_available)
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The soname that actually loaded (`libwebkit2gtk-4.1.so.0`, …).
    #[must_use]
    pub fn soname(&self) -> &'static str {
        self.soname
    }

    /// Put the shared web context into its lowest-memory cache model.
    ///
    /// A wallpaper is the degenerate browsing case: one document, loaded once,
    /// never navigated away from and never revisited. Webkit's default
    /// `WEB_BROWSER` model sizes its caches for the opposite workload, so the
    /// `DOCUMENT_VIEWER` model is strictly the right trade here.
    ///
    /// Must run before the first web view is created — the model is sampled
    /// when the web process starts. A no-op on a webkit that does not export
    /// these symbols.
    pub fn minimize_caches(&self) {
        let (Some(get_default), Some(set_model)) =
            (self.web_context_get_default, self.web_context_set_cache_model)
        else {
            return;
        };
        // SAFETY: both aliases transcribe the WebKitGTK headers; the default
        // context is owned by webkit (never unreffed here) and the model is a
        // plain enum value from the same headers.
        unsafe {
            let ctx = get_default();
            if !ctx.is_null() {
                set_model(ctx, CACHE_MODEL_DOCUMENT_VIEWER);
            }
        }
    }

    /// Create a `WebKitWebView` widget, optionally injecting `init_script`.
    ///
    /// `init_script` is installed on a fresh `WebKitUserContentManager` as a
    /// user script that runs at *document start* in *all frames*, which is the
    /// contract the WE JS bridge needs: `wallpaperRegisterAudioListener` & co.
    /// must exist before the page's own scripts run.
    ///
    /// Returns the widget as a raw `GtkWidget *` still holding the *floating*
    /// reference every `GInitiallyUnowned` is born with — the caller is
    /// expected to sink it immediately (glib's `from_glib_none` does). Returns
    /// null only if webkit itself failed to construct the view.
    ///
    /// The user content manager and the user script are intentionally not
    /// unreferenced: `webkit_user_script_unref` is not part of the bound
    /// symbol set, and this is a one-shot allocation in a process that hosts
    /// exactly one web view for its whole lifetime.
    pub fn new_web_view(&self, init_script: Option<&str>) -> *mut gtk::ffi::GtkWidget {
        // SAFETY: `webkit_user_content_manager_new` takes no arguments and
        // returns a new GObject; there is no precondition to uphold.
        let manager = unsafe { (self.user_content_manager_new)() };

        // A NUL inside the script would silently truncate it, so skip the
        // injection rather than inject a half script. Both bridge strings are
        // crate constants, so this cannot happen in practice.
        if let Some(source) = init_script {
            match CString::new(source) {
                Ok(source) => {
                    // SAFETY: `source` is a valid NUL-terminated C string that
                    // outlives the call (webkit copies it), and the two null
                    // pointers are the documented "no allow/block list" value.
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
                    // constructors and are still owned by us.
                    unsafe { (self.user_content_manager_add_script)(manager, script) };
                }
                Err(e) => tracing::warn!(error = %e, "init script contains a NUL; not injected"),
            }
        }

        // SAFETY: `manager` is a live `WebKitUserContentManager`; the web view
        // takes its own reference to it via the construct-only property.
        unsafe { (self.web_view_new_with_user_content_manager)(manager) }
    }

    /// Start loading `uri` in `view`.
    pub fn load_uri(&self, view: &gtk::Widget, uri: &str) {
        let Ok(uri) = CString::new(uri) else {
            tracing::error!("wallpaper url contains a NUL byte; not loading");
            return;
        };
        // SAFETY: see `as_web_view`; `uri` is a valid NUL-terminated C
        // string for the duration of the call and webkit copies it.
        unsafe { (self.web_view_load_uri)(as_web_view(view), uri.as_ptr()) };
    }

    /// Set the colour webkit paints where the page is transparent.
    ///
    /// `rgba` is normalised (0.0–1.0) red/green/blue/alpha.
    pub fn set_background_color(&self, view: &gtk::Widget, rgba: [f64; 4]) {
        let rgba = GdkRgba {
            red: rgba[0],
            green: rgba[1],
            blue: rgba[2],
            alpha: rgba[3],
        };
        // SAFETY: see `as_web_view`; `&rgba` is a valid `GdkRGBA` for the
        // duration of the call, which is all webkit borrows it for (it copies
        // the four doubles into the view).
        unsafe { (self.web_view_set_background_color)(as_web_view(view), &raw const rgba) };
    }

    /// Allow (or forbid) media playback that no user gesture asked for.
    ///
    /// WE web wallpapers start their `<video>`/`AudioContext` unprompted — the
    /// reference engine passes Chromium
    /// `--autoplay-policy=no-user-gesture-required`
    /// (docs/subsystems-misc.md §3.3) — so a wallpaper must be allowed to.
    /// wry expressed the same intent through `WebKitWebsitePolicies`
    /// (`AutoplayPolicy::Allow`, webkit 2.30+); the `WebKitSettings` property
    /// used here predates the 4.0/4.1 split entirely, which is what this
    /// binding needs.
    pub fn set_autoplay(&self, view: &gtk::Widget, allow: bool) {
        // SAFETY: see `as_web_view`. `webkit_web_view_get_settings` returns a
        // borrowed reference owned by the view; we do not keep it.
        let settings = unsafe { (self.web_view_get_settings)(as_web_view(view)) };
        if settings.is_null() {
            tracing::warn!("webkit view has no settings object; autoplay left at its default");
            return;
        }
        // SAFETY: `settings` is a live `WebKitSettings` owned by `view`, which
        // the caller keeps alive across this call.
        unsafe { (self.settings_set_media_gesture)(settings, c_int::from(!allow)) };

        // Same visit while we hold the settings object: the back/forward page
        // cache retains whole rendered documents so a *back* navigation is
        // instant. A wallpaper never navigates, so that memory can only ever
        // be dead weight.
        if let Some(set_page_cache) = self.settings_set_enable_page_cache {
            // SAFETY: same live `WebKitSettings` as above; the value is a
            // plain gboolean.
            unsafe { set_page_cache(settings, 0) };
        }

        // Workshop pages are file:// documents that fetch their own packaged
        // resources (data.json, music, subtitles) over XHR — same-origin for
        // a wallpaper, but webkit's default treats every file:// URL as a
        // distinct origin and silently denies it, leaving pages that load
        // their content dynamically stuck on an empty shell. The reference
        // engine runs CEF with `allow-file-access-from-files`.
        if let Some(set_file_access) = self.settings_set_allow_file_access {
            // SAFETY: same live `WebKitSettings`; plain gboolean.
            unsafe { set_file_access(settings, 1) };
        } else {
            tracing::warn!(
                "webkit lacks allow-file-access-from-file-urls; pages that XHR their own files will stay empty"
            );
        }
    }

    /// Run `js` in `view`'s main frame, discarding the result.
    ///
    /// Fire-and-forget by design: every caller is a bridge push (audio frames,
    /// property batches, synthetic pointer events) whose value we never read,
    /// so both spellings get a null `GCancellable`, callback and user data.
    /// Errors surface in webkit's own console rather than here, which matches
    /// the previous wry path — a broken page must never take the wallpaper
    /// down (SPEC V9).
    pub fn eval(&self, view: &gtk::Widget, js: &str) {
        let Ok(js) = CString::new(js) else {
            tracing::debug!("script contains a NUL byte; not evaluated");
            return;
        };
        let view = as_web_view(view);
        match self.eval {
            // SAFETY: see `as_web_view`. `js` outlives the call and webkit
            // copies it; `-1` is the documented "NUL-terminated, measure it
            // yourself" length; the null world name selects the page's own
            // JavaScript world and the null source URI leaves webkit to name
            // the script in stack traces.
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
            // older entry point does not take.
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

/// Reinterpret a web-view widget handle as the `WebKitWebView *` webkit wants.
///
/// `WebKitWebView` derives from `GtkWidget` through single GObject
/// inheritance, so the instance pointers are the same address and the cast is
/// a no-op — the same one the `WEBKIT_WEB_VIEW()` macro performs. Every caller
/// must pass a widget that came from [`WebKit::new_web_view`]; holding it as a
/// `&gtk::Widget` is what makes these calls safe, because that handle owns a
/// strong reference and therefore keeps the instance alive across the call.
fn as_web_view(view: &gtk::Widget) -> *mut c_void {
    view.as_ptr().cast::<c_void>()
}

/// Try each soname in [`SONAMES`] and bind the first one that has everything.
fn open_first_available() -> Result<WebKit, String> {
    let mut rejected = Vec::new();
    for soname in SONAMES {
        // SAFETY: `dlopen` runs the library's initialisers, which is the
        // unsafety `Library::new` is flagged for. WebKitGTK is a GTK3 library
        // whose initialisers are exactly what a compile-time link would have
        // run at process start, and the host has already called `gtk::init()`.
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

/// Resolve every entry point out of an already-opened library.
fn bind(lib: libloading::Library, soname: &'static str) -> Result<WebKit, String> {
    // Each `symbol` call asserts that the alias matches the C declaration
    // quoted on it; that pairing is the whole safety argument for this module.
    // SAFETY: the aliases above transcribe the WebKitGTK headers verbatim, and
    // the names are unchanged across the 4.0/4.1 API versions (the split is a
    // libsoup one, not a webkit one — see the module docs).
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
            // Prefer the modern entry point; fall back to the deprecated one
            // so pre-2.40 webkit (the common case on the 4.0 LTS packages)
            // still works. A build with neither is not WebKitGTK.
            eval: match symbol(&lib, b"webkit_web_view_evaluate_javascript\0") {
                Ok(f) => EvalFn::Evaluate(f),
                Err(_) => EvalFn::Run(symbol(&lib, b"webkit_web_view_run_javascript\0")?),
            },
            // Best-effort (see the field docs): `.ok()`, never `?`.
            web_context_get_default: symbol(&lib, b"webkit_web_context_get_default\0").ok(),
            web_context_set_cache_model: symbol(&lib, b"webkit_web_context_set_cache_model\0").ok(),
            settings_set_enable_page_cache: symbol(&lib, b"webkit_settings_set_enable_page_cache\0").ok(),
            settings_set_allow_file_access: symbol(
                &lib,
                b"webkit_settings_set_allow_file_access_from_file_urls\0",
            )
            .ok(),
            _lib: lib,
        }
    };
    Ok(webkit)
}

/// Resolve one symbol and copy the bare function pointer out of it.
///
/// # Safety
///
/// `T` must be the exact C signature `name` was declared with; calling a
/// pointer bound to a mismatched signature is undefined behaviour.
unsafe fn symbol<T: Copy>(lib: &libloading::Library, name: &'static [u8]) -> Result<T, String> {
    // SAFETY: delegated to this function's own contract. `Library::get`
    // borrows `lib`, but the value copied out is a plain function pointer that
    // stays valid for as long as the library stays loaded — and it is never
    // unloaded (see `WebKit::load`).
    let symbol: libloading::Symbol<'_, T> = unsafe { lib.get(name) }.map_err(|e| {
        // Trim the NUL the C lookup needs before it reaches a log line.
        let name = String::from_utf8_lossy(&name[..name.len() - 1]);
        format!("missing symbol {name} ({e})")
    })?;
    Ok(*symbol)
}
