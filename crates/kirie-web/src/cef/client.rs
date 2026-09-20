use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use cef::{
    Browser, Client, ImplClient, ImplLifeSpanHandler, ImplRenderHandler, LifeSpanHandler, PaintElementType,
    Rect, RenderHandler, WrapClient, WrapLifeSpanHandler, WrapRenderHandler, rc::Rc, wrap_client,
    wrap_life_span_handler, wrap_render_handler,
};

use crate::backend::{FrameBuffer, FrameSlot, PixelFormat};

#[derive(Debug)]
pub struct SharedSize {
    width: AtomicI32,
    height: AtomicI32,
}

impl SharedSize {
    #[must_use]
    pub fn new(width: i32, height: i32) -> Arc<Self> {
        Arc::new(Self {
            width: AtomicI32::new(width.max(1)),
            height: AtomicI32::new(height.max(1)),
        })
    }

    pub fn set(&self, width: i32, height: i32) {
        self.width.store(width.max(1), Ordering::Relaxed);
        self.height.store(height.max(1), Ordering::Relaxed);
    }

    #[must_use]
    pub fn width(&self) -> i32 {
        self.width.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn height(&self) -> i32 {
        self.height.load(Ordering::Relaxed)
    }
}

wrap_render_handler! {
    struct KirieRenderHandler {
        slot: FrameSlot,
        size: Arc<SharedSize>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.size.width();
                rect.height = self.size.height();
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW {
                return;
            }
            if buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            // Checked rather than plain arithmetic: a wrap on a 32-bit target
            // would ask for fewer bytes than the buffer holds and put a corrupt
            // frame on the screen.
            let Some(len) = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
            else {
                return;
            };
            // SAFETY: CEF guarantees `buffer` points to `width * height * 4`
            // bytes of BGRA with an upper-left origin, with no row padding, and
            // that it is valid for the duration of this call. `width` and
            // `height` are the ones it just handed us rather than any size held
            // elsewhere, so they cannot have drifted from the buffer, and both
            // were checked positive above. The bytes are copied out before the
            // call returns.
            let data = unsafe { std::slice::from_raw_parts(buffer, len) }.to_vec();
            let frame = FrameBuffer {
                data,
                width: width as u32,
                height: height as u32,
                format: PixelFormat::Bgra8,
            };
            self.slot.store(Some(Arc::new(frame)));
        }
    }
}

pub static LIVE_BROWSERS: AtomicUsize = AtomicUsize::new(0);

wrap_life_span_handler! {
    struct KirieLifeSpan;

    impl LifeSpanHandler {
        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            LIVE_BROWSERS.fetch_add(1, Ordering::SeqCst);
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            LIVE_BROWSERS.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

wrap_client! {
    struct KirieClient {
        handler: RenderHandler,
        life: LifeSpanHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.handler.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life.clone())
        }
    }
}

#[must_use]
pub fn make_client(slot: FrameSlot, size: Arc<SharedSize>) -> Client {
    let handler = KirieRenderHandler::new(slot, size);
    KirieClient::new(handler, KirieLifeSpan::new())
}
