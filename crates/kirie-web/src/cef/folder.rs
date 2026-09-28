//! Serves a wallpaper's own folder to its browser under
//! `https://wallpaper.kirie.invalid`, so the page never runs as `file://`.
//!
//! As a `file://` page it needed `--allow-file-access-from-files` and
//! `--disable-web-security` to fetch its own JSON and media, and with those any
//! wallpaper could read every file the user can and post it anywhere. Served
//! this way its own files are same-origin and nothing else on disk is
//! reachable: [`PageSource::resolve`] decides what a request may open.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::raw::c_int;
use std::sync::{Arc, Mutex, PoisonError};

use cef::{
    Browser, Callback, CefString, Frame, ImplRequest, ImplRequestContext, ImplResourceHandler, ImplResponse,
    ImplSchemeHandlerFactory, Request, RequestContext, RequestContextSettings, ResourceHandler,
    ResourceReadCallback, ResourceSkipCallback, Response, SchemeHandlerFactory, WrapResourceHandler,
    WrapSchemeHandlerFactory, rc::Rc, request_context_create_context, wrap_resource_handler,
    wrap_scheme_handler_factory,
};

use crate::page::{FOLDER_HOST, PageSource, mime_type};

/// Chromium's `net::ERR_FAILED`, what a read or skip reports when the file
/// cannot be read.
const ERR_FAILED: c_int = -2;

/// A request context of its own for a folder page, with the folder mapped
/// under [`FOLDER_HOST`]. Each browser gets one, so two wallpapers in one
/// process never see each other's files.
pub fn folder_context(source: &Arc<PageSource>) -> Option<RequestContext> {
    let context = request_context_create_context(Some(&RequestContextSettings::default()), None)?;
    let mut factory = FolderSchemeFactory::new(Arc::clone(source));
    let registered = context.register_scheme_handler_factory(
        Some(&CefString::from("https")),
        Some(&CefString::from(FOLDER_HOST)),
        Some(&mut factory),
    );
    (registered == 1).then_some(context)
}

struct Opened {
    file: File,
    len: u64,
    mime: &'static str,
}

fn open(source: &PageSource, url: &str) -> Option<Opened> {
    let path = source.resolve(url)?;
    let file = File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    Some(Opened {
        file,
        len,
        mime: mime_type(&path),
    })
}

wrap_scheme_handler_factory! {
    struct FolderSchemeFactory {
        source: Arc<PageSource>,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = request
                .map(|request| CefString::from(&request.url()).to_string())
                .unwrap_or_default();
            let opened = open(&self.source, &url);
            if opened.is_none() {
                tracing::debug!(url, "not in the wallpaper's folder");
            }
            Some(FolderResource::new(Arc::new(Mutex::new(opened))))
        }
    }
}

wrap_resource_handler! {
    struct FolderResource {
        opened: Arc<Mutex<Option<Opened>>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut c_int>,
            _callback: Option<&mut Callback>,
        ) -> c_int {
            if let Some(handle_request) = handle_request {
                *handle_request = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let Some(response) = response else { return };
            let opened = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
            let (status, text, mime, len) = match opened.as_ref() {
                Some(opened) => (200, "OK", opened.mime, i64::try_from(opened.len).unwrap_or(-1)),
                None => (404, "Not Found", "text/plain", 0),
            };
            response.set_status(status);
            response.set_status_text(Some(&CefString::from(text)));
            response.set_mime_type(Some(&CefString::from(mime)));
            if let Some(response_length) = response_length {
                *response_length = len;
            }
        }

        // CEF answers a Range request by skipping to its start, so media can
        // seek without this handler parsing the header itself.
        fn skip(
            &self,
            bytes_to_skip: i64,
            bytes_skipped: Option<&mut i64>,
            _callback: Option<&mut ResourceSkipCallback>,
        ) -> c_int {
            let Some(bytes_skipped) = bytes_skipped else { return 0 };
            let mut opened = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
            match opened.as_mut().map(|opened| opened.file.seek(SeekFrom::Current(bytes_to_skip))) {
                Some(Ok(_)) => {
                    *bytes_skipped = bytes_to_skip;
                    1
                }
                _ => {
                    *bytes_skipped = i64::from(ERR_FAILED);
                    0
                }
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: c_int,
            bytes_read: Option<&mut c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> c_int {
            let Some(bytes_read) = bytes_read else { return 0 };
            *bytes_read = 0;
            let len = usize::try_from(bytes_to_read).unwrap_or(0);
            let mut opened = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(opened) = opened.as_mut() else { return 0 };
            if data_out.is_null() || len == 0 {
                return 0;
            }
            // SAFETY: CEF passes a buffer it owns of `bytes_to_read` bytes,
            // valid and unaliased for the duration of this call.
            let out = unsafe { std::slice::from_raw_parts_mut(data_out, len) };
            match opened.file.read(out) {
                Ok(0) => 0,
                Ok(n) => {
                    *bytes_read = c_int::try_from(n).unwrap_or(0);
                    1
                }
                Err(_) => {
                    *bytes_read = ERR_FAILED;
                    0
                }
            }
        }
    }
}
