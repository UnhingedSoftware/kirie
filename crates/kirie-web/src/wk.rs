use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapImageRep, NSImage, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSError, NSPoint, NSRect, NSRunLoop, NSSize, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView, WKWebViewConfiguration};

use crate::backend::{FrameBuffer, OffscreenWeb, PixelFormat, WebError, WebSize};

const OFFSCREEN_ORIGIN: f64 = -32_000.0;
const PUMP: f64 = 0.008;

pub struct WkBackend {
    view: Retained<WKWebView>,
    window: Retained<NSWindow>,
    size: WebSize,
    frame: Option<FrameBuffer>,
    incoming: Arc<Mutex<Option<FrameBuffer>>>,
    waiting: Arc<Mutex<bool>>,
}

impl OffscreenWeb for WkBackend {
    fn open(url: &str, size: WebSize) -> Result<Self, WebError> {
        let size = size.clamped();
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| WebError::Init("WKWebView needs the main thread".to_owned()))?;

        let app = NSApplication::sharedApplication(mtm);
        if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }

        let rect = NSRect::new(
            NSPoint::new(OFFSCREEN_ORIGIN, OFFSCREEN_ORIGIN),
            NSSize::new(f64::from(size.width), f64::from(size.height)),
        );
        let view = make_view(mtm, rect);
        let window = make_window(mtm, rect);
        window.setContentView(Some(&view));

        load(&view, url)?;

        Ok(Self {
            view,
            window,
            size,
            frame: None,
            incoming: Arc::new(Mutex::new(None)),
            waiting: Arc::new(Mutex::new(false)),
        })
    }

    fn produces_frames(&self) -> bool {
        false
    }

    fn tick(&mut self, _dt: f32) {
        pump_runloop();

        if let Ok(mut slot) = self.incoming.lock()
            && let Some(frame) = slot.take()
        {
            self.frame = Some(frame);
        }

        let idle = self.waiting.lock().map(|flag| !*flag).unwrap_or(false);
        if idle {
            self.request_snapshot();
        }
    }

    fn apply_properties(&mut self, json: &str) {
        let script = format!(
            "window.wallpaperPropertyListener && window.wallpaperPropertyListener.applyUserProperties && window.wallpaperPropertyListener.applyUserProperties({json});"
        );
        self.evaluate(&script);
    }

    fn snapshot(&mut self) -> Option<FrameBuffer> {
        for _ in 0..SNAPSHOT_TRIES {
            self.tick(0.0);
            if let Ok(mut slot) = self.incoming.lock()
                && let Some(frame) = slot.take()
            {
                return Some(frame);
            }
        }
        self.frame.take()
    }

    fn shutdown(&mut self) {
        self.window.close();
    }
}

const SNAPSHOT_TRIES: usize = 240;

impl WkBackend {
    pub fn resize(&mut self, size: WebSize) {
        let size = size.clamped();
        if size == self.size {
            return;
        }
        self.size = size;
        let rect = NSRect::new(
            NSPoint::new(OFFSCREEN_ORIGIN, OFFSCREEN_ORIGIN),
            NSSize::new(f64::from(size.width), f64::from(size.height)),
        );
        self.window.setFrame_display(rect, false);
        self.view.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(size.width), f64::from(size.height)),
        ));
    }

    fn request_snapshot(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Ok(mut flag) = self.waiting.lock() else {
            return;
        };
        *flag = true;
        drop(flag);

        let incoming = Arc::clone(&self.incoming);
        let waiting = Arc::clone(&self.waiting);
        let handler = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
            if let Some(image) = (!image.is_null())
                .then(|| unsafe { Retained::retain(image) })
                .flatten()
                && let Some(frame) = frame_from_image(&image)
                && let Ok(mut slot) = incoming.lock()
            {
                *slot = Some(frame);
            }
            if let Ok(mut flag) = waiting.lock() {
                *flag = false;
            }
        });

        let config = unsafe { WKSnapshotConfiguration::new(mtm) };
        unsafe {
            self.view
                .takeSnapshotWithConfiguration_completionHandler(Some(&config), &handler);
        }
    }

    fn evaluate(&self, script: &str) {
        let source = NSString::from_str(script);
        unsafe {
            self.view.evaluateJavaScript_completionHandler(&source, None);
        }
    }
}

fn make_view(mtm: MainThreadMarker, rect: NSRect) -> Retained<WKWebView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    let body = NSRect::new(NSPoint::new(0.0, 0.0), rect.size);
    unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), body, &config) }
}

fn make_window(mtm: MainThreadMarker, rect: NSRect) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(true);
    window.setHasShadow(false);
    window
}

fn load(view: &WKWebView, url: &str) -> Result<(), WebError> {
    let text = NSString::from_str(url);
    let target = NSURL::URLWithString(&text).ok_or_else(|| WebError::Url(format!("not a url: {url}")))?;

    if url.starts_with("file://") {
        let root = target
            .URLByDeletingLastPathComponent()
            .unwrap_or_else(|| target.clone());
        // SAFETY: both URLs are owned here and outlive the call
        unsafe { view.loadFileURL_allowingReadAccessToURL(&target, &root) };
        return Ok(());
    }

    let request = NSURLRequest::requestWithURL(&target);
    // SAFETY: the request is owned here and outlives the call
    unsafe { view.loadRequest(&request) };
    Ok(())
}

fn pump_runloop() {
    let until = NSDate::dateWithTimeIntervalSinceNow(PUMP);
    NSRunLoop::currentRunLoop().runUntilDate(&until);
}

fn frame_from_image(image: &NSImage) -> Option<FrameBuffer> {
    let tiff = image.TIFFRepresentation()?;
    let rep = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)?;

    let width = u32::try_from(rep.pixelsWide()).ok()?;
    let height = u32::try_from(rep.pixelsHigh()).ok()?;
    let stride = usize::try_from(rep.bytesPerRow()).ok()?;
    let samples = rep.samplesPerPixel();
    let bits = rep.bitsPerPixel();
    if width == 0 || height == 0 || bits != 32 || samples < 3 {
        return None;
    }

    let pixels = rep.bitmapData();
    let row_bytes = width as usize * 4;
    let total = stride.checked_mul(height as usize)?;
    // SAFETY: `rep` owns `total` bytes of pixel data for as long as it is alive
    let source = unsafe { std::slice::from_raw_parts(pixels, total) };

    let mut data = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let at = row * stride;
        data.extend_from_slice(source.get(at..at + row_bytes)?);
    }

    Some(FrameBuffer {
        data,
        width,
        height,
        format: PixelFormat::Rgba8,
    })
}
