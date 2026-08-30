use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSScreen,
    NSWindow, NSWindowCollectionBehavior, NSWindowOcclusionState, NSWindowStyleMask,
};
use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
use objc2_foundation::NSDefaultRunLoopMode;
use raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};

use crate::backend::PresentOptions;
use crate::error::PlatformError;
use crate::gpu::Gpu;
use crate::renderer::{RenderTarget, Renderer, RendererFactory, SurfaceSize};

struct MacOutput {
    format: wgpu::TextureFormat,
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
    unplugged: bool,
    checked_power: Instant,
    orders: std::sync::mpsc::Sender<crate::renderer::RenderCommand>,
    incoming: std::sync::mpsc::Receiver<crate::renderer::RenderCommand>,
    speed: f32,
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

        let mut windows = Vec::new();
        for (screen, name) in chosen_screens(mtm, &options.screen_roots) {
            windows.push((desktop_window(mtm, &screen), pixel_size(&screen), name));
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
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
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

        let (orders, incoming) = std::sync::mpsc::channel();
        let mut platform = Self {
            outputs,
            gpu,
            app,
            make_renderer,
            frame_interval: frame_interval(options.fps),
            unplugged: on_battery(),
            checked_power: Instant::now(),
            orders,
            incoming,
            speed: options.playback_speed as f32,
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

    #[must_use]
    pub fn orders(&self) -> std::sync::mpsc::Sender<crate::renderer::RenderCommand> {
        self.orders.clone()
    }

    #[must_use]
    pub fn screen_names(&self) -> Vec<String> {
        self.outputs.iter().map(|output| output.name.clone()).collect()
    }

    fn take_orders(&mut self) {
        use crate::renderer::RenderCommand;

        while let Ok(order) = self.incoming.try_recv() {
            match order {
                RenderCommand::Build { screen, build, .. } | RenderCommand::Swap { screen, build, .. } => {
                    self.install(&screen, build)
                }
                RenderCommand::SwapLocal { screen, build_local } => {
                    let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
                    let Some(at) = self.output_at(&screen) else {
                        continue;
                    };
                    let (name, size, format) = self.shape_of(at);
                    let renderer = build_local(&device, &queue, format, &name, (size.width, size.height));
                    if let Some(output) = self.outputs.get_mut(at) {
                        output.renderer = Some(renderer);
                        output.last_frame = None;
                    }
                }
                RenderCommand::Install { screen, renderer, .. } => {
                    if let Some(at) = self.output_at(&screen)
                        && let Some(output) = self.outputs.get_mut(at)
                    {
                        output.renderer = Some(renderer);
                        output.last_frame = None;
                    }
                }
                RenderCommand::SetProperty {
                    screen,
                    key,
                    value,
                    structural,
                } => {
                    let Some(at) = self.output_at(&screen) else {
                        continue;
                    };
                    if let Some(output) = self.outputs.get_mut(at)
                        && let Some(renderer) = output.renderer.as_mut()
                        && renderer.set_property(&key, &value) == crate::PropertyImpact::NeedsRebuild
                    {
                        structural.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                RenderCommand::Screenshot { screen, capture } => {
                    let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
                    let Some(at) = self.output_at(&screen) else {
                        continue;
                    };
                    let (_, size, format) = self.shape_of(at);
                    if let Some(output) = self.outputs.get_mut(at)
                        && let Some(renderer) = output.renderer.as_mut()
                    {
                        capture(&device, &queue, renderer.as_mut(), size, format);
                    }
                }
                RenderCommand::SetFps(fps) => self.frame_interval = frame_interval(fps),
                RenderCommand::SetSpeed(speed) => self.speed = speed.max(0.0),
            }
        }
    }

    fn pace(&self) -> Duration {
        match self.unplugged.then(battery_fps).flatten() {
            Some(fps) => frame_interval(Some(fps)).max(self.frame_interval),
            None => self.frame_interval,
        }
    }

    fn install(&mut self, screen: &str, build: crate::renderer::BuildFn) {
        let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
        let Some(at) = self.output_at(screen) else {
            tracing::warn!(screen, "no such screen; ignoring");
            return;
        };
        let (name, size, format) = self.shape_of(at);
        let renderer = build(&device, &queue, format, &name, (size.width, size.height));
        if let Some(output) = self.outputs.get_mut(at) {
            output.renderer = Some(renderer);
            output.last_frame = None;
        }
    }

    fn output_at(&self, screen: &str) -> Option<usize> {
        if screen.is_empty() {
            return (!self.outputs.is_empty()).then_some(0);
        }
        self.outputs
            .iter()
            .position(|output| output.name == screen)
            .or_else(|| (!self.outputs.is_empty()).then_some(0))
    }

    fn shape_of(&self, at: usize) -> (String, SurfaceSize, wgpu::TextureFormat) {
        self.outputs.get(at).map_or_else(
            || {
                (
                    String::new(),
                    SurfaceSize { width: 1, height: 1 },
                    wgpu::TextureFormat::Bgra8UnormSrgb,
                )
            },
            |output| (output.name.clone(), output.physical_size, output.format),
        )
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
        ctx.format = config.format;
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

    fn draw(&mut self, index: usize) -> bool {
        let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
        let speed = self.speed;

        let Some(ctx) = self.outputs.get_mut(index) else {
            return false;
        };
        if !ctx.configured || !on_screen(&ctx.window) {
            return false;
        }
        if ctx.first_frame_presented && settled(ctx.renderer.as_deref()) {
            return false;
        }
        let Some(wgpu_surface) = &ctx.wgpu_surface else {
            return false;
        };

        let mut texture = wgpu_surface.get_current_texture();
        if matches!(
            texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
        ) {
            self.configure_swapchain(index);
            let Some(ctx) = self.outputs.get(index) else {
                return false;
            };
            let Some(wgpu_surface) = &ctx.wgpu_surface else {
                return false;
            };
            texture = wgpu_surface.get_current_texture();
        }

        let Some(ctx) = self.outputs.get_mut(index) else {
            return false;
        };
        let texture = match texture {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            other => {
                tracing::debug!(output = %ctx.name, status = ?other, "skipping frame");
                return false;
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

        renderer.render(&view, ctx.physical_size, dt * speed);
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
        true
    }

    fn pump_events(&self) {
        loop {
            let event = next_event(&self.app);
            let Some(event) = event else { break };
            self.app.sendEvent(&event);
        }
    }

    pub(crate) fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        let deadline = duration.map(|until| Instant::now() + until);
        let mut checked_screens = Instant::now();

        loop {
            let frame_start = Instant::now();
            self.pump_events();
            self.take_orders();

            if frame_start.duration_since(checked_screens) >= SCREEN_POLL {
                checked_screens = frame_start;
                self.resize_to_screens();
            }

            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                break;
            }

            if frame_start.duration_since(self.checked_power) >= POWER_POLL {
                self.checked_power = frame_start;
                self.unplugged = on_battery();
            }

            let mut drew = false;
            for index in 0..self.outputs.len() {
                drew |= self.draw(index);
            }

            let pace = if drew { self.pace() } else { IDLE_POLL };
            let elapsed = frame_start.elapsed();
            if elapsed < pace {
                std::thread::sleep(pace - elapsed);
            }
        }

        Ok(())
    }
}

pub struct DesktopSurface {
    window: Retained<NSWindow>,
    name: String,
    size: SurfaceSize,
}

impl DesktopSurface {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn size(&self) -> SurfaceSize {
        self.size
    }

    pub fn show(&self, view: &objc2_app_kit::NSView) {
        view.setFrame(self.window.contentLayoutRect());
        self.window.setContentView(Some(view));
        self.window.orderFrontRegardless();
    }
}

pub fn open_desktop(screen_roots: &[String]) -> Result<Vec<DesktopSurface>, PlatformError> {
    let mtm = MainThreadMarker::new().ok_or(PlatformError::NotMainThread)?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    finish_launching(&app);

    let screens = chosen_screens(mtm, screen_roots);
    if screens.is_empty() {
        return Err(PlatformError::NoCrtcs);
    }

    Ok(screens
        .into_iter()
        .map(|(screen, name)| DesktopSurface {
            size: pixel_size(&screen),
            window: desktop_window(mtm, &screen),
            name,
        })
        .collect())
}

pub fn pump_desktop_events() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    while let Some(event) = next_event(&app) {
        app.sendEvent(&event);
    }
}

fn chosen_screens(mtm: MainThreadMarker, roots: &[String]) -> Vec<(Retained<NSScreen>, String)> {
    let all: Vec<(Retained<NSScreen>, String)> = NSScreen::screens(mtm)
        .iter()
        .enumerate()
        .map(|(index, screen)| {
            let name = screen_name(&screen, index);
            (screen, name)
        })
        .collect();

    let asked: Vec<(Retained<NSScreen>, String)> = all
        .iter()
        .filter(|(_, name)| roots.is_empty() || roots.iter().any(|want| want == name))
        .cloned()
        .collect();

    if asked.is_empty() {
        tracing::warn!(asked = ?roots, "no screen matched; covering every screen instead");
        return all;
    }
    asked
}

#[allow(unsafe_code)]
fn next_event(app: &NSApplication) -> Option<Retained<NSEvent>> {
    // SAFETY: called on the main thread, which the caller established
    unsafe {
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, None, NSDefaultRunLoopMode, true)
    }
}

static BATTERY_FPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_battery_fps(fps: u32) {
    BATTERY_FPS.store(fps, std::sync::atomic::Ordering::Relaxed);
}

fn battery_fps() -> Option<u32> {
    let fps = BATTERY_FPS.load(std::sync::atomic::Ordering::Relaxed);
    (fps > 0).then_some(fps)
}

fn on_battery() -> bool {
    let asked = std::process::Command::new("pmset")
        .args(["-g", "ps"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(asked) = asked else { return false };
    String::from_utf8_lossy(&asked.stdout).contains("Battery Power")
}

const POWER_POLL: Duration = Duration::from_secs(20);

const IDLE_POLL: Duration = Duration::from_millis(250);

const SCREEN_POLL: Duration = Duration::from_secs(2);

fn settled(renderer: Option<&dyn Renderer>) -> bool {
    let Some(renderer) = renderer else {
        return false;
    };
    renderer.is_passive() || matches!(renderer.redraw_hint(), crate::RedrawHint::Static)
}

fn on_screen(window: &NSWindow) -> bool {
    window.occlusionState().contains(NSWindowOcclusionState::Visible)
}

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
    let raw_display_handle = RawDisplayHandle::AppKit(AppKitDisplayHandle::new());

    // SAFETY: the view outlives the surface because the window owns it for the run
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })
    }?;
    Ok(surface)
}
