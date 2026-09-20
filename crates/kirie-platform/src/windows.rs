//! Putting wallpapers on a Windows desktop.
//!
//! The shape is the macOS backend's: one borderless window per monitor, a wgpu
//! surface on each, and a loop that draws them and paces itself. What differs
//! is where the windows go. Windows has exactly one place a wallpaper can
//! live -- inside Explorer's own desktop window, behind the icons -- so
//! `win32::wallpaper_host` finds it and every window is a child of it. The
//! same place also has to be found again whenever Explorer restarts, which it
//! does on its own schedule and takes our windows' parent with it.

use std::time::{Duration, Instant};

use raw_window_handle::{RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle};

use crate::backend::PresentOptions;
use crate::error::PlatformError;
use crate::gpu::Gpu;
use crate::renderer::{RenderTarget, Renderer, RendererFactory, SurfaceSize};
use crate::win32::{self, Rect};

struct WinOutput {
    window: win32::Handle,
    name: String,
    rect: Rect,
    format: wgpu::TextureFormat,
    wgpu_surface: Option<wgpu::Surface<'static>>,
    configured: bool,
    renderer: Option<Box<dyn Renderer>>,
    last_frame: Option<Instant>,
    drawn_at: Option<Instant>,
    due_at: Option<Instant>,
    first_frame_presented: bool,
    covered: bool,
}

impl WinOutput {
    fn size(&self) -> SurfaceSize {
        SurfaceSize {
            width: self.rect.width(),
            height: self.rect.height(),
        }
    }
}

/// What makes the wallpaper stop while something else is on top of it.
struct PauseConfig {
    enabled: bool,
    only_active: bool,
    ignore: Vec<String>,
}

pub struct WindowsPlatform {
    outputs: Vec<WinOutput>,
    gpu: Gpu,
    host: win32::Handle,
    make_renderer: RendererFactory,
    frame_interval: Duration,
    pointer: bool,
    pause: PauseConfig,
    unplugged: bool,
    checked_power: Instant,
    orders: std::sync::mpsc::Sender<crate::renderer::RenderCommand>,
    incoming: std::sync::mpsc::Receiver<crate::renderer::RenderCommand>,
    speed: f32,
}

impl WindowsPlatform {
    pub(crate) fn connect_with(
        make_renderer: RendererFactory,
        options: PresentOptions,
    ) -> Result<Self, PlatformError> {
        win32::announce_dpi_awareness();

        let host = win32::wallpaper_host().ok_or(PlatformError::NoDesktopHost)?;
        let chosen = chosen_monitors(&options.screen_roots);
        if chosen.is_empty() {
            return Err(PlatformError::NoCrtcs);
        }

        let mut made = Vec::with_capacity(chosen.len());
        for monitor in chosen {
            match win32::desktop_window(host, monitor.rect, options.take_clicks) {
                Some(window) => made.push((window, monitor)),
                None => tracing::error!(output = %monitor.name, "no wallpaper window for this screen"),
            }
        }
        let Some((first, _)) = made.first() else {
            return Err(PlatformError::NoCrtcs);
        };

        let (gpu, first_surface) = bring_up_gpu(*first)?;
        let mut first_surface = Some(first_surface);

        let mut outputs = Vec::with_capacity(made.len());
        for (index, (window, monitor)) in made.into_iter().enumerate() {
            let wgpu_surface = if index == 0 {
                first_surface.take()
            } else {
                match create_surface(&gpu.instance, window) {
                    Ok(surface) => Some(surface),
                    Err(err) => {
                        tracing::error!(output = %monitor.name, %err, "surface creation failed");
                        None
                    }
                }
            };
            outputs.push(WinOutput {
                window,
                name: monitor.name,
                rect: monitor.rect,
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu_surface,
                configured: false,
                renderer: None,
                last_frame: None,
                drawn_at: None,
                due_at: None,
                first_frame_presented: false,
                covered: false,
            });
        }

        let (orders, incoming) = std::sync::mpsc::channel();
        let mut platform = Self {
            outputs,
            gpu,
            host,
            make_renderer,
            frame_interval: frame_interval(options.fps),
            pointer: options.pointer,
            pause: PauseConfig {
                enabled: options.fullscreen_pause,
                only_active: options.fullscreen_pause_only_active,
                ignore: options
                    .fullscreen_pause_ignore_appids
                    .iter()
                    .map(|name| name.to_ascii_lowercase())
                    .collect(),
            },
            unplugged: win32::on_battery(),
            checked_power: Instant::now(),
            orders,
            incoming,
            speed: options.playback_speed as f32,
        };
        for index in 0..platform.outputs.len() {
            platform.configure_swapchain(index);
        }

        tracing::info!(screens = platform.outputs.len(), "windows backend up");
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
                    self.install(&screen, build);
                }
                RenderCommand::SwapLocal { screen, build_local } => {
                    let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
                    let Some(at) = self.output_at(&screen) else {
                        continue;
                    };
                    let (name, size, format) = self.shape_of(at);
                    let position = self.origin_of(at);
                    let renderer = build_local(
                        &device,
                        &queue,
                        format,
                        &name,
                        (size.width, size.height),
                        position,
                    );
                    self.adopt(at, renderer);
                }
                RenderCommand::Install { screen, renderer, .. } => {
                    let Some(at) = self.output_at(&screen) else {
                        continue;
                    };
                    self.adopt(at, renderer);
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

    fn install(&mut self, screen: &str, build: crate::renderer::BuildFn) {
        let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
        let Some(at) = self.output_at(screen) else {
            tracing::warn!(screen, "no such screen; ignoring");
            return;
        };
        let (name, size, format) = self.shape_of(at);
        let position = self.origin_of(at);
        let renderer = build(
            &device,
            &queue,
            format,
            &name,
            (size.width, size.height),
            position,
        );
        self.adopt(at, renderer);
    }

    /// Put a freshly built wallpaper on a screen, unless building it failed --
    /// in which case what is already up is better than nothing.
    fn adopt(&mut self, at: usize, renderer: Box<dyn Renderer>) {
        let Some(output) = self.outputs.get_mut(at) else {
            return;
        };
        if renderer.is_placeholder() {
            tracing::error!(screen = %output.name, "keeping the wallpaper that is up");
            return;
        }
        output.renderer = Some(renderer);
        output.last_frame = None;
        output.due_at = None;
        output.first_frame_presented = false;
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

    fn origin_of(&self, at: usize) -> (i32, i32) {
        self.outputs
            .get(at)
            .map_or((0, 0), |output| (output.rect.left, output.rect.top))
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
            |output| (output.name.clone(), output.size(), output.format),
        )
    }

    fn configure_swapchain(&mut self, index: usize) {
        let Some(output) = self.outputs.get_mut(index) else {
            return;
        };
        let Some(surface) = &output.wgpu_surface else {
            return;
        };
        let size = SurfaceSize {
            width: output.rect.width(),
            height: output.rect.height(),
        };
        let Some(mut config) = surface.get_default_config(&self.gpu.adapter, size.width, size.height) else {
            tracing::error!(output = %output.name, "adapter cannot present to this surface");
            return;
        };
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&self.gpu.device, &config);
        output.format = config.format;
        output.configured = true;
    }

    /// Explorer restarting takes the wallpaper host down and our windows with
    /// it, which is ordinary rather than exceptional: it happens on a crash, on
    /// a settings change, and whenever the user restarts it by hand. Finding
    /// the host again and re-parenting is all that is needed.
    fn keep_host(&mut self) {
        if win32::alive(self.host) && self.outputs.iter().all(|output| win32::alive(output.window)) {
            return;
        }
        let Some(host) = win32::wallpaper_host() else {
            tracing::warn!("the desktop has no host window right now; waiting for Explorer");
            return;
        };
        self.host = host;
        tracing::info!("the desktop window came back; putting the wallpapers back into it");

        for index in 0..self.outputs.len() {
            let Some(output) = self.outputs.get(index) else {
                continue;
            };
            if win32::alive(output.window) {
                win32::reparent(output.window, host);
                win32::place(output.window, output.rect);
                continue;
            }
            self.rebuild_window(index, host);
        }
    }

    /// Make a screen's window again after Explorer destroyed it, and give it a
    /// new surface. The renderer itself survives; only its canvas was lost.
    fn rebuild_window(&mut self, index: usize, host: win32::Handle) {
        let Some(output) = self.outputs.get(index) else {
            return;
        };
        let (rect, name) = (output.rect, output.name.clone());
        let Some(window) = win32::desktop_window(host, rect, false) else {
            tracing::error!(output = %name, "could not make the wallpaper window again");
            return;
        };
        let surface = match create_surface(&self.gpu.instance, window) {
            Ok(surface) => Some(surface),
            Err(err) => {
                tracing::error!(output = %name, %err, "no surface on the new window");
                None
            }
        };
        if let Some(output) = self.outputs.get_mut(index) {
            output.window = window;
            output.wgpu_surface = surface;
            output.configured = false;
            output.first_frame_presented = false;
        }
        self.configure_swapchain(index);
    }

    /// Follow the monitors being rearranged, resized or unplugged.
    fn resize_to_screens(&mut self) {
        let live = win32::monitors();
        for index in 0..self.outputs.len() {
            let Some(output) = self.outputs.get(index) else {
                continue;
            };
            let Some(monitor) = live.iter().find(|monitor| monitor.name == output.name) else {
                continue;
            };
            if monitor.rect == output.rect {
                continue;
            }
            let rect = monitor.rect;
            let window = output.window;
            if let Some(output) = self.outputs.get_mut(index) {
                output.rect = rect;
            }
            win32::place(window, rect);
            self.configure_swapchain(index);
        }
    }

    // The wallpaper is told where the pointer is even when it does not take
    // clicks: hover and parallax want the position, not the button.
    fn follow_pointer(&mut self) {
        let Some((at, held)) = win32::pointer() else {
            return;
        };
        for output in &mut self.outputs {
            let Some(renderer) = output.renderer.as_mut() else {
                continue;
            };
            let rect = output.rect;
            let x = (at.0 - rect.left) as f32 / rect.width() as f32;
            let y = (at.1 - rect.top) as f32 / rect.height() as f32;
            renderer.set_pointer_on_output(rect.contains(at));
            renderer.set_pointer(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
            renderer.set_pointer_buttons(held);
        }
    }

    /// Note which screens have something full-screen on them, so those screens
    /// can be left alone.
    ///
    /// Wayland answers this with `zwlr_foreign_toplevel`, which reports every
    /// window's state and which output it is on. Windows has no such list, but
    /// it does have the one window the user is actually in, and a window that
    /// covers its whole monitor is the case worth catching: a game, a video
    /// player, a presentation. Drawing a wallpaper underneath one costs frames
    /// the thing in front wants.
    fn notice_whats_on_top(&mut self) {
        if !self.pause.enabled {
            for output in &mut self.outputs {
                output.covered = false;
            }
            return;
        }

        let front = win32::foreground_window().filter(|(window, rect)| {
            // A window is only in the way if it covers the monitor it is on.
            win32::monitor_of(*window).is_some_and(|monitor| covers(*rect, monitor))
                && !self.is_ours(*window)
                && !self.ignored(*window)
        });

        let Some((_, rect)) = front else {
            for output in &mut self.outputs {
                output.covered = false;
            }
            return;
        };

        for output in &mut self.outputs {
            // `--fullscreen-pause-only-active` asks for the screen with the
            // full-screen window on it to pause and the others to carry on;
            // without it, one full-screen window quiets every screen.
            output.covered = if self.pause.only_active {
                covers(rect, output.rect)
            } else {
                true
            };
        }
    }

    fn is_ours(&self, window: win32::Handle) -> bool {
        self.outputs.iter().any(|output| output.window == window)
    }

    fn ignored(&self, window: win32::Handle) -> bool {
        if self.pause.ignore.is_empty() {
            return false;
        }
        let Some(program) = win32::program_of(window) else {
            return false;
        };
        self.pause
            .ignore
            .iter()
            .any(|wanted| program == *wanted || program.trim_end_matches(".exe") == wanted)
    }

    fn pace(&self) -> Duration {
        match self.unplugged.then(battery_fps).flatten() {
            Some(fps) => frame_interval(Some(fps)).max(self.frame_interval),
            None => self.frame_interval,
        }
    }

    fn draw(&mut self, index: usize) -> bool {
        let (device, queue) = (self.gpu.device.clone(), self.gpu.queue.clone());
        let speed = self.speed;

        let Some(output) = self.outputs.get_mut(index) else {
            return false;
        };
        if !output.configured {
            return false;
        }
        // A wallpaper spends its life under other windows, so being covered is
        // ordinary: slow down, never stop. A renderer that has not been shown
        // yet always gets its frame, so the desktop is never left blank.
        let shown = output.first_frame_presented;
        if shown {
            if settled(output.renderer.as_deref()) {
                return false;
            }
            // A renderer that asked to be left alone until a moment is left alone.
            if output.due_at.is_some_and(|due| Instant::now() < due) {
                return false;
            }
            if output.covered && output.drawn_at.is_some_and(|at| at.elapsed() < HIDDEN_FRAME) {
                return false;
            }
        }

        let Some(wgpu_surface) = &output.wgpu_surface else {
            return false;
        };
        let mut texture = wgpu_surface.get_current_texture();
        if matches!(
            texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
        ) {
            self.configure_swapchain(index);
            let Some(output) = self.outputs.get(index) else {
                return false;
            };
            let Some(wgpu_surface) = &output.wgpu_surface else {
                return false;
            };
            texture = wgpu_surface.get_current_texture();
        }

        let Some(output) = self.outputs.get_mut(index) else {
            return false;
        };
        let texture = match texture {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            other => {
                tracing::debug!(output = %output.name, status = ?other, "skipping frame");
                return false;
            }
        };

        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let fresh = output.renderer.is_none();
        let size = output.size();
        let origin = (output.rect.left, output.rect.top);
        let renderer = output.renderer.get_or_insert_with(|| {
            (self.make_renderer)(&RenderTarget {
                device: &device,
                queue: &queue,
                format: texture.texture.format(),
                output_name: &output.name,
                size: (size.width, size.height),
                position: origin,
            })
        });
        if fresh && renderer.is_placeholder() {
            tracing::error!(
                output = %output.name,
                "the wallpaper could not be built; this screen stays empty"
            );
        }

        let now = Instant::now();
        let dt = output
            .last_frame
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.0);
        output.last_frame = Some(now);

        renderer.render(&view, size, dt * speed);
        let asked = renderer.redraw_hint();
        queue.present(texture);
        output.drawn_at = Some(now);
        output.due_at = match asked {
            crate::RedrawHint::After(wait) => Some(now + wait),
            crate::RedrawHint::Static | crate::RedrawHint::Unknown => None,
        };

        if !output.first_frame_presented {
            output.first_frame_presented = true;
            tracing::info!(
                output = %output.name,
                width = size.width,
                height = size.height,
                "first frame presented"
            );
        }
        true
    }

    pub(crate) fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        let deadline = duration.map(|until| Instant::now() + until);
        let mut checked_screens = Instant::now();

        loop {
            let frame_start = Instant::now();
            win32::pump_messages();
            self.take_orders();

            if frame_start.duration_since(checked_screens) >= SCREEN_POLL {
                checked_screens = frame_start;
                self.keep_host();
                self.resize_to_screens();
                self.notice_whats_on_top();
            }

            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                break;
            }

            if frame_start.duration_since(self.checked_power) >= POWER_POLL {
                self.checked_power = frame_start;
                self.unplugged = win32::on_battery();
            }

            if self.pointer {
                self.follow_pointer();
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

impl Drop for WindowsPlatform {
    fn drop(&mut self) {
        // Explorer owns the parent, so a window left behind stays on the
        // desktop as a dead grey rectangle until the next restart.
        for output in &mut self.outputs {
            output.renderer = None;
            output.wgpu_surface = None;
            win32::destroy(output.window);
        }
    }
}

fn chosen_monitors(roots: &[String]) -> Vec<win32::Monitor> {
    let all = win32::monitors();
    if roots.is_empty() {
        return all;
    }
    let asked: Vec<win32::Monitor> = all
        .into_iter()
        .filter(|monitor| roots.iter().any(|want| want.eq_ignore_ascii_case(&monitor.name)))
        .collect();
    if asked.is_empty() {
        tracing::warn!(asked = ?roots, "no screen matched; covering every screen instead");
        return win32::monitors();
    }
    asked
}

/// Whether `window` covers the whole of `monitor`.
///
/// A borderless full-screen game matches the monitor exactly; an exclusive
/// full-screen one can report a rectangle a pixel or two larger. Anything
/// smaller is a window the wallpaper can keep playing behind.
fn covers(window: Rect, monitor: Rect) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

static BATTERY_FPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_battery_fps(fps: u32) {
    BATTERY_FPS.store(fps, std::sync::atomic::Ordering::Relaxed);
}

fn battery_fps() -> Option<u32> {
    let fps = BATTERY_FPS.load(std::sync::atomic::Ordering::Relaxed);
    (fps > 0).then_some(fps)
}

const POWER_POLL: Duration = Duration::from_secs(20);

const IDLE_POLL: Duration = Duration::from_millis(250);

const HIDDEN_FRAME: Duration = Duration::from_secs(1);

const SCREEN_POLL: Duration = Duration::from_secs(2);

fn settled(renderer: Option<&dyn Renderer>) -> bool {
    let Some(renderer) = renderer else {
        return false;
    };
    renderer.is_passive() || matches!(renderer.redraw_hint(), crate::RedrawHint::Static)
}

fn frame_interval(fps: Option<u32>) -> Duration {
    match fps.filter(|rate| *rate > 0) {
        Some(rate) => Duration::from_secs_f64(1.0 / f64::from(rate)),
        None => Duration::from_micros(16_666),
    }
}

fn bring_up_gpu(window: win32::Handle) -> Result<(Gpu, wgpu::Surface<'static>), PlatformError> {
    // DX12 first because it is what a Windows driver is tuned for; Vulkan is
    // there for the machines whose DX12 driver is the worse of the two.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12 | wgpu::Backends::VULKAN,
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
        label: Some("kirie-platform-windows"),
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
    window: win32::Handle,
) -> Result<wgpu::Surface<'static>, PlatformError> {
    let handle = win32::as_raw(window).ok_or(PlatformError::NoDesktopHost)?;
    let mut win32_handle = Win32WindowHandle::new(
        std::num::NonZeroIsize::new(handle.as_ptr() as isize).ok_or(PlatformError::NoDesktopHost)?,
    );
    win32_handle.hinstance = std::num::NonZeroIsize::new(win32::module_handle() as isize);

    // SAFETY: the window outlives the surface because the backend destroys its
    // surfaces before its windows, and Explorer only ever destroys a window
    // whose surface `keep_host` then replaces.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
            raw_window_handle: RawWindowHandle::Win32(win32_handle),
        })
    }?;
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn a_full_screen_window_covers_its_monitor() {
        assert!(covers(SCREEN, SCREEN));
    }

    #[test]
    fn an_exclusive_full_screen_window_can_overhang() {
        let bigger = Rect {
            left: -1,
            top: -1,
            right: 1921,
            bottom: 1081,
        };
        assert!(covers(bigger, SCREEN));
    }

    #[test]
    fn an_ordinary_window_does_not() {
        let windowed = Rect {
            left: 100,
            top: 100,
            right: 1820,
            bottom: 980,
        };
        assert!(!covers(windowed, SCREEN));
    }

    #[test]
    fn a_maximised_window_on_the_next_monitor_does_not_cover_this_one() {
        let other = Rect {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        assert!(!covers(other, SCREEN));
        assert!(covers(other, other));
    }

    #[test]
    fn an_unrequested_fps_is_sixty() {
        assert_eq!(frame_interval(None), Duration::from_micros(16_666));
        assert_eq!(frame_interval(Some(0)), Duration::from_micros(16_666));
        assert_eq!(frame_interval(Some(30)), Duration::from_secs_f64(1.0 / 30.0));
    }
}
