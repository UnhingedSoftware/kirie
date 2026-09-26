//! Web wallpapers on Windows, through Microsoft Edge WebView2.
//!
//! This is the macOS `wk` module's counterpart: a browser view that sits inside
//! the wallpaper's own window rather than drawing into a wgpu texture. The page
//! runs in Edge's own processes and composites itself, so once it is up nothing
//! here draws a frame.
//!
//! WebView2 is not part of kirie. It is a system component: installed with
//! every Windows 11 and, through Edge's updater, on nearly every Windows 10,
//! and downloadable on its own from Microsoft for the rest. [`runtime_version`]
//! is how to tell before trying.
//!
//! Every call here has to come from the thread that owns the parent window,
//! and that thread has to be a single-threaded COM apartment that pumps its
//! messages: WebView2 delivers every completion by posting to it. The Windows
//! backend's render loop is exactly that.

use std::path::Path;
use std::sync::mpsc;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_COLOR, COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW,
    CreateCoreWebView2EnvironmentWithOptions, GetAvailableCoreWebView2BrowserVersionString, ICoreWebView2,
    ICoreWebView2_3, ICoreWebView2_8, ICoreWebView2Controller, ICoreWebView2Controller2,
    ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, CoTaskMemPWSTR, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
};
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use windows::core::{HSTRING, Interface as _, PCWSTR, PWSTR};

use crate::backend::{WebError, WebSize};
use crate::page::{FOLDER_HOST, PageSource};

/// Where Microsoft publishes the WebView2 installer, for messages that tell a
/// user what to fetch.
pub const RUNTIME_DOWNLOAD: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// The installed WebView2 runtime's version, or `None` when there is none.
///
/// Asking costs a registry read inside the loader and starts nothing, so it is
/// cheap enough to do before refusing a web wallpaper outright.
#[must_use]
pub fn runtime_version() -> Option<String> {
    let mut found = PWSTR::null();
    // SAFETY: a null browser folder asks for the installed runtime, and
    // `found` is an out-parameter the loader fills with a CoTaskMem string or
    // leaves null. `CoTaskMemPWSTR` takes ownership and frees it.
    let asked = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut found) };
    let owned = CoTaskMemPWSTR::from(found);
    asked.ok()?;
    let version = owned.to_string();
    (!version.is_empty()).then_some(version)
}

/// A web page filling one wallpaper window.
///
/// Dropping it closes the view and lets the window be drawn on again.
pub struct DesktopPage {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

impl DesktopPage {
    /// Open `source` in a view covering `parent`.
    ///
    /// `parent` is the wallpaper window's `HWND`, as an address. `init` runs
    /// in every document before the page's own scripts: it is where the
    /// Wallpaper Engine bridge, the initial properties and the volume go.
    /// `data` is where WebView2 keeps its profile, which must be writable.
    ///
    /// Blocks, pumping this thread's messages, until the view exists.
    pub fn open(
        parent: isize,
        size: WebSize,
        source: &PageSource,
        init: &str,
        muted: bool,
        data: &Path,
    ) -> Result<Self, WebError> {
        let parent = HWND(parent as *mut core::ffi::c_void);
        if parent.is_invalid() {
            return Err(WebError::Init("no window to put the page in".to_owned()));
        }
        if runtime_version().is_none() {
            return Err(WebError::Init(format!(
                "the Microsoft Edge WebView2 Runtime is not installed; get it from {RUNTIME_DOWNLOAD}"
            )));
        }

        let environment = {
            let options = CoreWebView2EnvironmentOptions::default();
            // Wallpapers start playing on their own; there is never a click to
            // unlock sound with, so Chromium's autoplay gate would keep every
            // one silent for good.
            // SAFETY: the options object is ours and not shared with WebView2
            // until the conversion below.
            unsafe {
                options.set_additional_browser_arguments(
                    "--autoplay-policy=no-user-gesture-required".to_owned(),
                );
            }
            let options: ICoreWebView2EnvironmentOptions = options.into();
            let folder = HSTRING::from(data.as_os_str());
            let _ = std::fs::create_dir_all(data);

            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| {
                    // SAFETY: the folder string and the options outlive the
                    // call, a null browser folder means the installed runtime,
                    // and the handler is a live COM object.
                    unsafe {
                        CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &folder, &options, &handler)
                    }
                    .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |outcome, environment| {
                    outcome?;
                    let _ = tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                    Ok(())
                }),
            )
            .map_err(failed)?;
            rx.recv().map_err(failed)?.map_err(failed)?
        };

        let controller = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| {
                    // SAFETY: `parent` was checked above and is a window this
                    // thread owns; the handler is a live COM object.
                    unsafe { environment.CreateCoreWebView2Controller(parent, &handler) }
                        .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |outcome, controller| {
                    outcome?;
                    let _ = tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                    Ok(())
                }),
            )
            .map_err(failed)?;
            rx.recv().map_err(failed)?.map_err(failed)?
        };

        // SAFETY: the controller was just created on this thread.
        let webview = unsafe { controller.CoreWebView2() }.map_err(failed)?;
        let page = Self { controller, webview };
        page.settle(size, muted)?;
        page.run_first(init)?;
        page.navigate(source)?;
        Ok(page)
    }

    /// The window's size changed; the view follows it.
    pub fn resize(&self, size: WebSize) {
        let size = size.clamped();
        let bounds = RECT {
            left: 0,
            top: 0,
            right: i32::try_from(size.width).unwrap_or(i32::MAX),
            bottom: i32::try_from(size.height).unwrap_or(i32::MAX),
        };
        // SAFETY: the controller is alive for as long as `self` is.
        if let Err(err) = unsafe { self.controller.SetBounds(bounds) } {
            tracing::warn!(%err, "could not resize the page");
        }
    }

    /// Stop drawing while nobody can see it, and pick up again after.
    ///
    /// A hidden WebView2 throttles its timers and animation frames, which is
    /// what a wallpaper under a full-screen game should do.
    pub fn set_hidden(&self, hidden: bool) {
        // SAFETY: the controller is alive for as long as `self` is.
        if let Err(err) = unsafe { self.controller.SetIsVisible(!hidden) } {
            tracing::debug!(%err, "could not change the page's visibility");
        }
    }

    fn settle(&self, size: WebSize, muted: bool) -> Result<(), WebError> {
        self.resize(size);
        // SAFETY: the controller and view are alive and on this thread.
        unsafe {
            // Black until the page paints, rather than a white flash across the
            // whole desktop.
            if let Ok(controller) = self.controller.cast::<ICoreWebView2Controller2>() {
                let _ = controller.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                    A: 255,
                    R: 0,
                    G: 0,
                    B: 0,
                });
            }
            let settings = self.webview.Settings().map_err(failed)?;
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
            let _ = settings.SetAreDevToolsEnabled(false);
            let _ = settings.SetIsStatusBarEnabled(false);
            let _ = settings.SetIsZoomControlEnabled(false);
            let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
            if muted && let Ok(view) = self.webview.cast::<ICoreWebView2_8>() {
                let _ = view.SetIsMuted(true);
            }
            self.controller.SetIsVisible(true).map_err(failed)?;
        }
        Ok(())
    }

    fn run_first(&self, init: &str) -> Result<(), WebError> {
        let webview = self.webview.clone();
        let script = HSTRING::from(init);
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                // SAFETY: the script string outlives the call and the handler
                // is a live COM object.
                unsafe { webview.AddScriptToExecuteOnDocumentCreated(&script, &handler) }
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(|outcome, _id| outcome),
        )
        .map_err(failed)
    }

    fn navigate(&self, source: &PageSource) -> Result<(), WebError> {
        if let PageSource::Folder { dir, .. } = source {
            let mapped = self
                .webview
                .cast::<ICoreWebView2_3>()
                .map_err(|_| WebError::Init("this WebView2 Runtime is too old; update it".to_owned()))?;
            let host = HSTRING::from(FOLDER_HOST);
            let folder = HSTRING::from(dir.as_os_str());
            // SAFETY: both strings outlive the call.
            unsafe {
                mapped.SetVirtualHostNameToFolderMapping(
                    &host,
                    &folder,
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW,
                )
            }
            .map_err(failed)?;
        }
        let address = source.address();
        let target = HSTRING::from(address.as_str());
        // SAFETY: the address string outlives the call.
        unsafe { self.webview.Navigate(&target) }
            .map_err(|err| WebError::Url(format!("{address}: {err}")))?;
        tracing::info!(%address, "opened the page");
        Ok(())
    }
}

impl kirie_platform::PageView for DesktopPage {
    fn resize(&mut self, size: kirie_platform::SurfaceSize) {
        Self::resize(
            self,
            WebSize {
                width: size.width,
                height: size.height,
            },
        );
    }

    fn set_hidden(&mut self, hidden: bool) {
        Self::set_hidden(self, hidden);
    }
}

impl Drop for DesktopPage {
    fn drop(&mut self) {
        // SAFETY: closing a controller this thread made; WebView2 tears down
        // its child window and lets go of the browser process.
        if let Err(err) = unsafe { self.controller.Close() } {
            tracing::debug!(%err, "closing the page");
        }
    }
}

fn failed(err: impl std::fmt::Display) -> WebError {
    WebError::Init(format!("WebView2: {err}"))
}
