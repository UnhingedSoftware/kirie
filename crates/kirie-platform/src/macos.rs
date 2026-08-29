use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSScreen,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
use objc2_foundation::NSDefaultRunLoopMode;
use raw_window_handle::{AppKitWindowHandle, RawWindowHandle};

use crate::backend::PresentOptions;
use crate::error::PlatformError;
use crate::gpu::Gpu;
use crate::renderer::{RenderTarget, Renderer, RendererFactory, SurfaceSize};

struct MacOutput {
    wgpu_surface: Option<wgpu::Surface<'static>>,
    window: Retained<NSWindow>,
    name: String,
    physical_size: SurfaceSize,
    configured: bool,
    renderer: Option<Box<dyn Renderer>>,
    last_frame: Option<Instant>,
    first_frame_presented: bool,
}

pub struct MacPlatform {
    outputs: Vec<MacOutput>,
    gpu: Gpu,
    app: Retained<NSApplication>,
    make_renderer: RendererFactory,
    frame_interval: Duration,
}

impl MacPlatform {
    pub(crate) fn connect_with(
        make_renderer: RendererFactory,
        options: PresentOptions,
    ) -> Result<Self, PlatformError> {
        let mtm = MainThreadMarker::new().ok_or(PlatformError::NotMainThread)?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        finish_launching(&app);

        let screens = NSScreen::screens(mtm);
        let wanted: Vec<(Retained<NSScreen>, String)> = screens
            .iter()
            .enumerate()
            .map(|(index, screen)| {
                let name = screen_name(&screen, index);
                (screen, name)
            })
            .collect();
        let asked: Vec<&(Retained<NSScreen>, String)> = wanted
            .iter()
            .filter(|(_, name)| {
                options.screen_roots.is_empty() || options.screen_roots.iter().any(|want| want == name)
            })
            .collect();
        let chosen = if asked.is_empty() {
            tracing::warn!(
                asked = ?options.screen_roots,
                "no screen matched; covering every screen instead"
            );
            wanted.iter().collect()
        } else {
            asked
        };

        let mut windows = Vec::new();
        for (screen, name) in chosen {
            windows.push((desktop_window(mtm, screen), pixel_size(screen), name.clone()));
        }
        if windows.is_empty() {
            return Err(PlatformError::NoCrtcs);
        }

        let (gpu, first_surface) = bring_up_gpu(&windows[0].0)?;
        let mut first_surface = Some(first_surface);

        let mut outputs = Vec::with_capacity(windows.len());
        for (index, (window, size, name)) in windows.into_iter().enumerate() {
            let wgpu_surface = if index == 0 {
                first_surface.take()
            } else {
                match create_surface(&gpu.instance, &window) {
                    Ok(surface) => Some(surface),
                    Err(err) => {
                        tracing::error!(output = %name, %err, "metal surface creation failed");
                        None
                    }
                }
            };
            outputs.push(MacOutput {
                wgpu_surface,
                window,
                name,
                physical_size: size,
                configured: false,
                renderer: None,
                last_frame: None,
                first_frame_presented: false,
            });
        }

        let mut platform = Self {
            outputs,
            gpu,
            app,
            make_renderer,
            frame_interval: frame_interval(options.fps),
        };
        for index in 0..platform.outputs.len() {
            platform.configure_swapchain(index);
        }

        tracing::info!(screens = platform.outputs.len(), "macos backend up");
        Ok(platform)
    }

    #[must_use]
    pub(crate) fn output_count(&self) -> usize {
        self.outputs.len()
    }

    #[must_use]
    pub(crate) fn surface_count(&self) -> usize {
        self.outputs.len()
    }

    fn configure_swapchain(&mut self, index: usize) {
        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        let Some(surface) = &ctx.wgpu_surface else {
            return;
        };
        let Some(mut config) = surface.get_default_config(
            &self.gpu.adapter,
            ctx.physical_size.width,
            ctx.physical_size.height,
        ) else {
            tracing::error!(output = %ctx.name, "adapter cannot present to this surface");
            return;
        };
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&self.gpu.device, &config);
        ctx.configured = true;
    }

    fn resize_to_screens(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let screens = NSScreen::screens(mtm);
        for (index, screen) in screens.iter().enumerate() {
            let name = screen_name(&screen, index);
            let Some(at) = self.outputs.iter().position(|output| output.name == name) else {
                continue;
            };
            let size = pixel_size(&screen);
            let Some(output) = self.outputs.get_mut(at) else {
                continue;
            };
            if output.physical_size == size {
                continue;
            }
            output.window.setFrame_display(screen.frame(), true);
            output.physical_size = size;
            self.configure_swapchain(at);
        }
    }

    fn draw(&mut self, index: usize) {
        let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());

        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        if !ctx.configured {
            return;
        }
        let Some(wgpu_surface) = &ctx.wgpu_surface else {
            return;
        };

        let mut texture = wgpu_surface.get_current_texture();
        if matches!(
            texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
        ) {
            self.configure_swapchain(index);
            let Some(ctx) = self.outputs.get(index) else {
                return;
            };
            let Some(wgpu_surface) = &ctx.wgpu_surface else {
                return;
            };
            texture = wgpu_surface.get_current_texture();
        }

        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        let texture = match texture {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            other => {
                tracing::debug!(output = %ctx.name, status = ?other, "skipping frame");
                return;
            }
        };

        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let renderer = ctx.renderer.get_or_insert_with(|| {
            (self.make_renderer)(&RenderTarget {
                device: &device,
                queue: &queue,
                format: texture.texture.format(),
                output_name: &ctx.name,
                size: (ctx.physical_size.width, ctx.physical_size.height),
            })
        });

        let now = Instant::now();
        let dt = ctx
            .last_frame
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.0);
        ctx.last_frame = Some(now);

        renderer.render(&view, ctx.physical_size, dt);
        queue.present(texture);

        if !ctx.first_frame_presented {
            ctx.first_frame_presented = true;
            tracing::info!(
                output = %ctx.name,
                width = ctx.physical_size.width,
                height = ctx.physical_size.height,
                "first frame presented"
            );
        }
    }

    fn pump_events(&self) {
        loop {
            let event = self.next_event();
            let Some(event) = event else { break };
            self.app.sendEvent(&event);
        }
    }

    #[allow(unsafe_code)]
    fn next_event(&self) -> Option<Retained<NSEvent>> {
        // SAFETY: called on the main thread, which `connect_with` established
        unsafe {
            self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                None,
                NSDefaultRunLoopMode,
                true,
            )
        }
    }

    pub(crate) fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        let deadline = duration.map(|until| Instant::now() + until);
        let mut checked_screens = Instant::now();

        loop {
            let frame_start = Instant::now();
            self.pump_events();

            if frame_start.duration_since(checked_screens) >= SCREEN_POLL {
                checked_screens = frame_start;
                self.resize_to_screens();
            }

            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                break;
            }

            for index in 0..self.outputs.len() {
                self.draw(index);
            }

            let elapsed = frame_start.elapsed();
            if elapsed < self.frame_interval {
                std::thread::sleep(self.frame_interval - elapsed);
            }
        }

        Ok(())
    }
}

const SCREEN_POLL: Duration = Duration::from_secs(2);

fn frame_interval(fps: Option<u32>) -> Duration {
    match fps.filter(|rate| *rate > 0) {
        Some(rate) => Duration::from_secs_f64(1.0 / f64::from(rate)),
        None => Duration::from_micros(16_666),
    }
}

fn screen_name(screen: &NSScreen, index: usize) -> String {
    let name = screen.localizedName().to_string();
    if name.is_empty() {
        format!("Screen-{index}")
    } else {
        name
    }
}

fn pixel_size(screen: &NSScreen) -> SurfaceSize {
    let frame = screen.frame();
    let scale = screen.backingScaleFactor();
    SurfaceSize {
        width: (frame.size.width * scale).max(1.0) as u32,
        height: (frame.size.height * scale).max(1.0) as u32,
    }
}

fn desktop_window(mtm: MainThreadMarker, screen: &NSScreen) -> Retained<NSWindow> {
    let window = desktop_window_alloc(mtm, screen);

    window.setLevel(desktop_level());
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    window.setIgnoresMouseEvents(true);
    window.setOpaque(true);
    window.setHasShadow(false);
    if let Some(view) = window.contentView() {
        view.setWantsLayer(true);
    }
    window.orderFrontRegardless();
    window
}

#[allow(unsafe_code)]
fn desktop_window_alloc(mtm: MainThreadMarker, screen: &NSScreen) -> Retained<NSWindow> {
    // SAFETY: an AppKit initializer called on the main thread
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer_screen(
            NSWindow::alloc(mtm),
            screen.frame(),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
            Some(screen),
        )
    };
    // SAFETY: the window is owned here and never closed by AppKit itself
    unsafe { window.setReleasedWhenClosed(false) };
    window
}

fn finish_launching(app: &NSApplication) {
    app.finishLaunching();
}

fn desktop_level() -> isize {
    CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey) as isize
}

fn bring_up_gpu(window: &NSWindow) -> Result<(Gpu, wgpu::Surface<'static>), PlatformError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    let surface = create_surface(&instance, window)?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        power_preference: crate::gpu::power_preference(),
        ..wgpu::RequestAdapterOptions::default()
    }))?;

    let info = adapter.get_info();
    tracing::info!(backend = %info.backend, adapter = %info.name, "selected gpu adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("kirie-platform-macos"),
        ..wgpu::DeviceDescriptor::default()
    }))?;

    Ok((
        Gpu {
            instance,
            adapter,
            device,
            queue,
        },
        surface,
    ))
}

#[allow(unsafe_code)]
fn create_surface(
    instance: &wgpu::Instance,
    window: &NSWindow,
) -> Result<wgpu::Surface<'static>, PlatformError> {
    let view = window
        .contentView()
        .ok_or_else(|| PlatformError::MacWindow("window has no content view".to_string()))?;
    let pointer = NonNull::new(Retained::as_ptr(&view).cast_mut().cast())
        .ok_or_else(|| PlatformError::MacWindow("content view pointer was null".to_string()))?;
    let raw_window_handle = RawWindowHandle::AppKit(AppKitWindowHandle::new(pointer));

    // SAFETY: the view outlives the surface because the window owns it for the run
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: None,
            raw_window_handle,
        })
    }?;
    Ok(surface)
}
