use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::channel::{
    Event as CalloopEvent, Sender as CmdSender, channel,
};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use std::collections::HashMap;

use crate::renderer::RenderCommand;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{self, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, registry_handlers,
};
use smithay_client_toolkit::{delegate_pointer, delegate_seat};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_surface};
use wayland_client::protocol::{wl_pointer, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use crate::error::PlatformError;
use crate::gpu::Gpu;
use crate::output::OutputContext;
use crate::renderer::{RenderTarget, RendererFactory, SurfaceSize};
use crate::toplevel::{PauseConfig, ToplevelTracker};

const PAUSE_WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);

const PASSIVE_POLL_INTERVAL: Duration = Duration::from_millis(33);

pub struct WaylandPlatform {
    event_loop: EventLoop<'static, PlatformState>,
    state: PlatformState,
    cmd_tx: CmdSender<RenderCommand>,
}

struct PlatformState {
    outputs: Vec<OutputContext>,
    gpu: Option<Gpu>,
    conn: Connection,
    qh: QueueHandle<PlatformState>,

    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,

    make_renderer: RendererFactory,
    layer: Layer,
    namespace: String,
    screen_roots: Vec<String>,
    min_frame: Option<std::time::Duration>,
    playback_speed: f32,
    cmd_tx: CmdSender<RenderCommand>,
    preloaded: HashMap<(String, String), (wgpu::TextureFormat, Box<dyn crate::renderer::Renderer + Send>)>,
    pointer: crate::pointer::PointerPoll,
    seat_state: SeatState,
    cursor_shape: Option<WpCursorShapeManagerV1>,
    pointers: Vec<(wl_pointer::WlPointer, Option<WpCursorShapeDeviceV1>)>,
    buttons: crate::pointer::PointerButtons,
    initial_build: Option<crate::renderer::InitialBuildFn>,
    loop_handle: LoopHandle<'static, PlatformState>,
    toplevels: ToplevelTracker,
    release_hidden_after: Option<Duration>,
    activity_paused: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pause_watchdog_armed: bool,
    passive_poll_armed: bool,
    _toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
}

impl WaylandPlatform {
    pub fn connect(make_renderer: RendererFactory) -> Result<Self, PlatformError> {
        Self::connect_with(make_renderer, crate::PresentOptions::default())
    }

    pub fn connect_with(
        make_renderer: RendererFactory,
        options: crate::PresentOptions,
    ) -> Result<Self, PlatformError> {
        let conn = Connection::connect_to_env()?;
        let (globals, event_queue) = registry_queue_init::<PlatformState>(&conn)?;
        let qh = event_queue.handle();

        let compositor_state = CompositorState::bind(&globals, &qh)?;
        let layer_shell = LayerShell::bind(&globals, &qh)?;
        let output_state = OutputState::new(&globals, &qh);
        let seat_state = SeatState::new(&globals, &qh);
        let cursor_shape: Option<WpCursorShapeManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let registry_state = RegistryState::new(&globals);

        let event_loop = EventLoop::try_new()?;
        WaylandSource::new(conn.clone(), event_queue)
            .insert(event_loop.handle())
            .map_err(|err| PlatformError::EventLoopRegister(err.to_string()))?;

        let (cmd_tx, cmd_rx) = channel::<RenderCommand>();
        event_loop
            .handle()
            .insert_source(cmd_rx, |event, _, state: &mut PlatformState| {
                if let CalloopEvent::Msg(cmd) = event {
                    state.handle_command(cmd);
                }
            })
            .map_err(|err| PlatformError::EventLoopRegister(err.to_string()))?;

        let loop_handle = event_loop.handle();

        let mut toplevels = ToplevelTracker::new(PauseConfig {
            enabled: options.fullscreen_pause,
            only_active: options.fullscreen_pause_only_active,
            ignore_appids: options.fullscreen_pause_ignore_appids,
        });
        let toplevel_manager = if toplevels.enabled() {
            match globals.bind::<ZwlrForeignToplevelManagerV1, PlatformState, _>(&qh, 2..=3, ()) {
                Ok(manager) => {
                    toplevels.set_supported();
                    Some(manager)
                }
                Err(err) => {
                    tracing::debug!(
                        %err,
                        "compositor has no zwlr_foreign_toplevel_manager_v1; fullscreen pause disabled"
                    );
                    None
                }
            }
        } else {
            tracing::debug!("--no-fullscreen-pause: not tracking foreign toplevels");
            None
        };

        Ok(Self {
            event_loop,
            cmd_tx: cmd_tx.clone(),
            state: PlatformState {
                outputs: Vec::new(),
                gpu: None,
                conn,
                qh,
                registry_state,
                output_state,
                compositor_state,
                layer_shell,
                make_renderer,
                layer: Layer::Background,
                namespace: options.layer_namespace,
                screen_roots: options.screen_roots,
                release_hidden_after: options.release_hidden_after,
                activity_paused: options.activity_paused.clone(),
                min_frame: options
                    .fps
                    .filter(|f| *f > 0)
                    .map(|f| std::time::Duration::from_secs_f64(1.0 / f64::from(f))),
                playback_speed: if options.playback_speed > 0.0 {
                    options.playback_speed as f32
                } else {
                    1.0
                },
                cmd_tx,
                preloaded: HashMap::new(),
                pointer: crate::pointer::PointerPoll::start(),
                seat_state,
                cursor_shape,
                pointers: Vec::new(),
                buttons: crate::pointer::PointerButtons::default(),
                initial_build: None,
                loop_handle,
                toplevels,
                pause_watchdog_armed: false,
                passive_poll_armed: false,
                _toplevel_manager: toplevel_manager,
            },
        })
    }

    pub fn set_initial_build(&mut self, f: crate::renderer::InitialBuildFn) {
        self.state.initial_build = Some(f);
    }

    #[must_use]
    pub fn command_sender(&self) -> CmdSender<RenderCommand> {
        self.cmd_tx.clone()
    }

    #[must_use]
    pub fn output_count(&self) -> usize {
        self.state.outputs.len()
    }

    #[must_use]
    pub fn surface_count(&self) -> usize {
        self.state
            .outputs
            .iter()
            .filter(|o| o.wgpu_surface.is_some())
            .count()
    }

    pub fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        let deadline = duration.map(|d| Instant::now() + d);

        loop {
            let timeout = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    Some(deadline - now)
                }
                None => None,
            };

            self.event_loop.dispatch(timeout, &mut self.state)?;
        }

        tracing::info!("run deadline reached; tearing down surfaces");
        Ok(())
    }
}

impl PlatformState {
    fn handle_command(&mut self, cmd: RenderCommand) {
        match cmd {
            RenderCommand::Build { screen, stash, build } => {
                let Some(gpu) = &self.gpu else { return };
                let Some(ctx) = self.output_for(&screen) else {
                    return;
                };
                let Some(format) = ctx.format else { return };
                let name = ctx.name.clone();
                let size = (ctx.physical_size.width, ctx.physical_size.height);
                let position = ctx.position;
                let device = gpu.device.clone();
                let queue = gpu.queue.clone();
                let tx = self.cmd_tx.clone();
                std::thread::spawn(move || {
                    let renderer = build(&device, &queue, format, &name, size, position);
                    let _ = tx.send(RenderCommand::Install {
                        screen: name,
                        stash,
                        renderer,
                    });
                });
            }
            RenderCommand::Install {
                screen,
                stash,
                renderer,
            } => {
                let Some(idx) = self.output_index(&screen) else {
                    return;
                };
                match stash {
                    Some(key) => {
                        let name = self.outputs[idx].name.clone();
                        if let Some(format) = self.outputs[idx].format {
                            self.preloaded.retain(|(n, _), _| *n != name);
                            self.preloaded.insert((name, key), (format, renderer));
                            kirie_bake::trim_heap();
                        }
                    }
                    None => self.install_renderer(idx, renderer),
                }
            }
            RenderCommand::Swap { screen, key, build } => {
                let Some(idx) = self.output_index(&screen) else {
                    return;
                };
                let name = self.outputs[idx].name.clone();
                let hit = self
                    .preloaded
                    .remove(&(name, key))
                    .and_then(|(format, r)| (self.outputs[idx].format == Some(format)).then_some(r));
                match hit {
                    Some(renderer) => {
                        tracing::info!(%screen, "preload hit — instant swap");
                        self.install_renderer(idx, renderer);
                    }
                    None => {
                        tracing::info!(%screen, "preload miss — building off-thread");
                        self.handle_command(RenderCommand::Build {
                            screen,
                            stash: None,
                            build,
                        });
                    }
                }
            }
            RenderCommand::SetProperty {
                screen,
                key,
                value,
                structural,
            } => {
                let Some(idx) = self.output_index(&screen) else {
                    return;
                };
                let ctx = &mut self.outputs[idx];
                if let Some(renderer) = ctx.renderer.as_mut() {
                    let impact = renderer.set_property(&key, &value);
                    structural.store(
                        impact == crate::renderer::PropertyImpact::NeedsRebuild,
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    if ctx.configured && !ctx.frame_pending {
                        let qh = self.qh.clone();
                        ctx.wl_surface().frame(&qh, ctx.wl_surface().clone());
                        ctx.frame_pending = true;
                        ctx.wl_surface().commit();
                    }
                }
            }
            RenderCommand::SwapLocal { screen, build_local } => {
                let Some(gpu) = &self.gpu else { return };
                let Some(idx) = self.output_index(&screen) else {
                    return;
                };
                let Some(format) = self.outputs[idx].format else {
                    return;
                };
                let name = self.outputs[idx].name.clone();
                let size = (
                    self.outputs[idx].physical_size.width,
                    self.outputs[idx].physical_size.height,
                );
                let device = gpu.device.clone();
                let queue = gpu.queue.clone();
                let position = self.outputs[idx].position;
                let renderer = build_local(&device, &queue, format, &name, size, position);
                self.install_renderer(idx, renderer);
            }
            RenderCommand::SetFps(fps) => {
                self.min_frame = fps
                    .filter(|f| *f > 0)
                    .map(|f| std::time::Duration::from_secs_f64(1.0 / f64::from(f)));
                let surfaces: Vec<_> = self
                    .outputs
                    .iter()
                    .filter(|c| c.configured && c.renderer.is_some() && !c.paused)
                    .map(|c| c.wl_surface().clone())
                    .collect();
                for surface in surfaces {
                    self.draw(&surface);
                }
            }
            RenderCommand::SetSpeed(speed) => {
                self.playback_speed = if speed > 0.0 { speed } else { 1.0 };
            }
            RenderCommand::Screenshot { screen, capture } => {
                let Some(gpu) = &self.gpu else { return };
                let Some(idx) = self.output_index(&screen) else {
                    return;
                };
                let ctx = &mut self.outputs[idx];
                let Some(format) = ctx.format else { return };
                let size = ctx.physical_size;
                if let Some(renderer) = ctx.renderer.as_mut() {
                    capture(&gpu.device, &gpu.queue, renderer.as_mut(), size, format);
                }
            }
        }
    }

    fn install_renderer(&mut self, idx: usize, renderer: Box<dyn crate::renderer::Renderer>) {
        let ctx = &mut self.outputs[idx];
        ctx.renderer = Some(renderer);
        let was_initial = std::mem::take(&mut ctx.initial_build_pending);
        ctx.last_frame = None;
        let configured = ctx.configured;
        let surface = ctx.wl_surface().clone();
        kirie_bake::trim_heap();
        kirie_bake::pageout_cold_libs();
        if let Some(gpu) = &self.gpu {
            crate::gpu::persist_pipeline_cache(&gpu.adapter);
        }
        tracing::info!(output = %self.outputs[idx].name, "wallpaper swapped in");

        if configured {
            self.draw(&surface);
        } else if was_initial {
            tracing::debug!("launch build landed before configure; deferring first paint");
        }
    }

    fn output_for(&self, screen: &str) -> Option<&OutputContext> {
        if screen == "*" {
            self.outputs.first()
        } else {
            self.outputs.iter().find(|c| c.name == screen)
        }
    }

    fn output_index(&self, screen: &str) -> Option<usize> {
        if screen == "*" {
            return (!self.outputs.is_empty()).then_some(0);
        }
        if let Some(found) = self.outputs.iter().position(|c| c.name == screen) {
            return Some(found);
        }
        (self.outputs.len() == 1).then_some(0)
    }

    fn add_output(&mut self, qh: &QueueHandle<Self>, wl_output: wl_output::WlOutput) {
        let info = self.output_state.info(&wl_output);
        let name = info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default();
        let scale = info
            .as_ref()
            .map(|i| u32::try_from(i.scale_factor.max(1)).unwrap_or(1))
            .unwrap_or(1);
        let position = info.as_ref().map_or((0, 0), |i| (i.location.0, i.location.1));

        if !self.screen_roots.is_empty() && !self.screen_roots.iter().any(|r| r == &name) {
            tracing::info!(
                output = %name,
                requested = ?self.screen_roots,
                "output not in requested --screen-root list; leaving it alone"
            );
            return;
        }

        tracing::info!(output = %name, scale, namespace = %self.namespace, "new output; creating layer surface");

        let surface = self.compositor_state.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            self.layer,
            Some(self.namespace.clone()),
            Some(&wl_output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        layer.wl_surface().commit();

        let wgpu_surface = match &self.gpu {
            Some(gpu) => gpu.create_surface(&self.conn, layer.wl_surface()),
            None => Gpu::new_for_surface(&self.conn, layer.wl_surface()).map(|(gpu, surface)| {
                self.gpu = Some(gpu);
                surface
            }),
        };

        let wgpu_surface = match wgpu_surface {
            Ok(surface) => Some(surface),
            Err(err) => {
                tracing::error!(output = %name, %err, "gpu surface creation failed");
                None
            }
        };

        self.outputs.push(OutputContext {
            wgpu_surface,
            layer,
            wl_output,
            name,
            scale,
            logical_size: (0, 0),
            physical_size: SurfaceSize { width: 1, height: 1 },
            configured: false,
            frame_pending: false,
            timer_armed: false,
            static_content: false,
            paused: false,
            paused_at: None,
            released: false,
            initial_build_pending: false,
            first_frame_presented: false,
            renderer: None,
            last_frame: None,
            format: None,
            position,
        });

        self.apply_pause();
    }

    fn apply_pause(&mut self) {
        let mut resumed = Vec::new();
        for index in 0..self.outputs.len() {
            let blocking = self
                .toplevels
                .blocking_appid(&self.outputs[index].wl_output)
                .map(str::to_owned);
            let ctx = &mut self.outputs[index];
            match (ctx.paused, blocking) {
                (false, Some(appid)) => {
                    ctx.paused = true;
                    ctx.paused_at = Some(Instant::now());
                    tracing::info!(output = %ctx.name, %appid, "wallpaper paused");
                }
                (true, None) => {
                    ctx.paused = false;
                    ctx.paused_at = None;
                    ctx.released = false;
                    ctx.frame_pending = false;
                    tracing::info!(output = %ctx.name, "wallpaper resumed");
                    resumed.push(ctx.wl_surface().clone());
                }
                _ => {}
            }
        }

        for surface in resumed {
            self.draw(&surface);
        }

        self.update_pointer_demand();
        if let Some(flag) = &self.activity_paused {
            let any = self.outputs.iter().any(|c| c.paused);
            flag.store(any, std::sync::atomic::Ordering::Relaxed);
        }
        self.ensure_pause_watchdog();
    }

    fn release_hidden_outputs(&mut self) {
        let Some(after) = self.release_hidden_after else {
            return;
        };
        let gpu = self.gpu.as_ref().map(|g| (g.device.clone(), g.queue.clone()));
        for index in 0..self.outputs.len() {
            let ctx = &mut self.outputs[index];
            if !ctx.paused || ctx.released || ctx.renderer.is_none() {
                continue;
            }
            if !ctx.paused_at.is_some_and(|at| at.elapsed() >= after) {
                continue;
            }
            let name = ctx.name.clone();
            let still = ctx.renderer.as_mut().and_then(|r| r.snapshot());
            ctx.renderer = None;
            ctx.released = true;
            let stood_in = match (&still, &gpu, &ctx.wgpu_surface) {
                (Some(still), Some((device, queue)), Some(surface)) => {
                    crate::snapshot::present_still(device, queue, surface, still)
                }
                _ => false,
            };
            drop(still);
            kirie_bake::trim_heap();
            kirie_bake::pageout_cold_libs();
            tracing::info!(
                output = %name,
                still = stood_in,
                "released hidden wallpaper; will rebuild when visible"
            );
        }
    }

    fn ensure_pause_watchdog(&mut self) {
        if self.pause_watchdog_armed || !self.outputs.iter().any(|ctx| ctx.paused) {
            return;
        }
        self.pause_watchdog_armed = true;
        let timer = Timer::from_duration(PAUSE_WATCHDOG_INTERVAL);
        let armed = self
            .loop_handle
            .insert_source(timer, |_, _, state: &mut PlatformState| {
                state.apply_pause();
                state.release_hidden_outputs();
                if state.outputs.iter().any(|ctx| ctx.paused) {
                    TimeoutAction::ToDuration(PAUSE_WATCHDOG_INTERVAL)
                } else {
                    state.pause_watchdog_armed = false;
                    TimeoutAction::Drop
                }
            });
        if let Err(err) = armed {
            self.pause_watchdog_armed = false;
            tracing::warn!(%err, "could not arm the fullscreen-pause watchdog");
        }
    }

    fn needs_passive_poll(&self) -> bool {
        self.outputs
            .iter()
            .any(|ctx| !ctx.paused && !ctx.released && ctx.renderer.as_ref().is_some_and(|r| r.is_passive()))
    }

    fn poll_passive(&mut self) {
        for ctx in &mut self.outputs {
            if ctx.paused || ctx.released {
                continue;
            }
            if let Some(renderer) = ctx.renderer.as_mut()
                && renderer.is_passive()
            {
                renderer.poll();
            }
        }
    }

    fn ensure_passive_poll(&mut self) {
        if self.passive_poll_armed || !self.needs_passive_poll() {
            return;
        }
        self.passive_poll_armed = true;
        let timer = Timer::from_duration(PASSIVE_POLL_INTERVAL);
        let armed = self
            .loop_handle
            .insert_source(timer, |_, _, state: &mut PlatformState| {
                state.poll_passive();
                if state.needs_passive_poll() {
                    TimeoutAction::ToDuration(PASSIVE_POLL_INTERVAL)
                } else {
                    state.passive_poll_armed = false;
                    TimeoutAction::Drop
                }
            });
        if let Err(err) = armed {
            self.passive_poll_armed = false;
            tracing::warn!(%err, "could not arm the passive-renderer poll; web pages get no live data");
        }
    }

    fn configure_swapchain(&mut self, index: usize) {
        let Some(gpu) = &self.gpu else { return };
        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        let Some(surface) = &ctx.wgpu_surface else {
            return;
        };

        let Some(mut config) =
            surface.get_default_config(&gpu.adapter, ctx.physical_size.width, ctx.physical_size.height)
        else {
            tracing::error!(output = %ctx.name, "adapter cannot present to this surface");
            return;
        };
        config.present_mode = wgpu::PresentMode::Fifo;
        ctx.format = Some(config.format);
        surface.configure(&gpu.device, &config);

        if let Ok(region) = Region::new(&self.compositor_state) {
            region.add(
                0,
                0,
                i32::try_from(ctx.logical_size.0).unwrap_or(i32::MAX),
                i32::try_from(ctx.logical_size.1).unwrap_or(i32::MAX),
            );
            ctx.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        ctx.configured = true;
    }

    fn draw(&mut self, surface: &wl_surface::WlSurface) {
        let Some(index) = self.outputs.iter().position(|ctx| ctx.wl_surface() == surface) else {
            return;
        };

        let Some(gpu) = &self.gpu else { return };
        let (device, queue) = (gpu.device.clone(), gpu.queue.clone());

        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        if !ctx.configured {
            return;
        }

        if ctx.paused {
            return;
        }

        if let (Some(min), Some(prev)) = (self.min_frame, ctx.last_frame) {
            let elapsed = prev.elapsed();
            if elapsed < min {
                if !ctx.timer_armed {
                    ctx.timer_armed = true;
                    let surface = ctx.wl_surface().clone();
                    let timer = Timer::from_duration(min - elapsed);
                    let _ = self.loop_handle.insert_source(timer, move |_, _, state| {
                        if let Some(ctx) = state.outputs.iter_mut().find(|c| c.wl_surface() == &surface) {
                            ctx.timer_armed = false;
                        }
                        state.draw(&surface);
                        TimeoutAction::Drop
                    });
                }
                return;
            }
        }

        if self.outputs[index].renderer.is_none() {
            if self.outputs[index].initial_build_pending {
                return;
            }
            let name = self.outputs[index].name.clone();
            if self.outputs[index].format.is_some()
                && let Some(build) = self.initial_build.as_mut().and_then(|f| f(&name))
            {
                self.outputs[index].initial_build_pending = true;
                tracing::debug!(output = %name, "building launch wallpaper off-thread");
                self.handle_command(RenderCommand::Build {
                    screen: name,
                    stash: None,
                    build,
                });
                return;
            }
        }

        {
            let ctx = &self.outputs[index];
            if ctx.first_frame_presented && ctx.renderer.as_ref().is_some_and(|r| r.is_passive()) {
                self.ensure_passive_poll();
                return;
            }
        }

        let ctx = &mut self.outputs[index];

        let Some(wgpu_surface) = &ctx.wgpu_surface else {
            return;
        };

        let mut texture = wgpu_surface.get_current_texture();

        if matches!(
            texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
        ) {
            tracing::debug!(output = %ctx.name, "swapchain outdated/lost; reconfiguring");
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
                tracing::debug!(output = %ctx.name, status = ?other, "skipping frame; re-arming callback");
                if !ctx.frame_pending {
                    ctx.wl_surface().frame(&self.qh, ctx.wl_surface().clone());
                    ctx.frame_pending = true;
                }
                ctx.wl_surface().commit();
                return;
            }
        };

        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        ctx.format = Some(texture.texture.format());

        let renderer = ctx.renderer.get_or_insert_with(|| {
            (self.make_renderer)(&RenderTarget {
                device: &device,
                queue: &queue,
                format: texture.texture.format(),
                output_name: &ctx.name,
                size: (ctx.physical_size.width, ctx.physical_size.height),
                position: ctx.position,
            })
        });

        let now = Instant::now();
        let dt = ctx
            .last_frame
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.0)
            * self.playback_speed;
        ctx.last_frame = Some(now);

        if let Some((gx, gy)) = self.pointer.get() {
            let (lw, lh) = ctx.logical_size;
            if lw > 0 && lh > 0 {
                let nx = ((gx - f64::from(ctx.position.0)) / f64::from(lw)).clamp(0.0, 1.0);
                let ny = ((gy - f64::from(ctx.position.1)) / f64::from(lh)).clamp(0.0, 1.0);
                renderer.set_pointer(nx as f32, ny as f32);
            }
        }
        renderer.set_pointer_buttons(self.buttons.left());

        renderer.render(&view, ctx.physical_size, dt);
        let hint = renderer.redraw_hint();

        if self.min_frame.is_none() && !ctx.frame_pending && hint == crate::renderer::RedrawHint::Unknown {
            ctx.wl_surface().frame(&self.qh, ctx.wl_surface().clone());
            ctx.frame_pending = true;
        }

        queue.present(texture);

        if !ctx.first_frame_presented {
            ctx.first_frame_presented = true;
            tracing::info!(
                output = %ctx.name,
                width = ctx.physical_size.width,
                height = ctx.physical_size.height,
                "first frame presented"
            );
            kirie_bake::trim_heap();
            kirie_bake::pageout_cold_libs();
            if let Some(gpu) = &self.gpu {
                crate::gpu::persist_pipeline_cache(&gpu.adapter);
            }
        }

        let next = match hint {
            crate::renderer::RedrawHint::Static => None,
            crate::renderer::RedrawHint::After(d) => {
                Some(d.max(self.min_frame.unwrap_or(std::time::Duration::ZERO)))
            }
            crate::renderer::RedrawHint::Unknown => self.min_frame,
        };
        if let Some(wait) = next {
            let ctx = &mut self.outputs[index];
            if !ctx.timer_armed {
                ctx.timer_armed = true;
                let surface = ctx.wl_surface().clone();
                let timer = Timer::from_duration(wait);
                let _ = self.loop_handle.insert_source(timer, move |_, _, state| {
                    if let Some(ctx) = state.outputs.iter_mut().find(|c| c.wl_surface() == &surface) {
                        ctx.timer_armed = false;
                    }
                    state.draw(&surface);
                    TimeoutAction::Drop
                });
            }
        } else if hint == crate::renderer::RedrawHint::Static {
            tracing::debug!(output = %self.outputs[index].name, "static content; frame scheduling stopped");
        }
        self.outputs[index].static_content = hint == crate::renderer::RedrawHint::Static;
        self.update_pointer_demand();
    }

    fn update_pointer_demand(&self) {
        let live = self
            .outputs
            .iter()
            .any(|c| c.renderer.is_some() && !c.paused && !c.static_content);
        self.pointer.set_active(live);
    }

    fn apply_scale(&mut self, surface: &wl_surface::WlSurface, new_scale: u32) {
        let Some(index) = self.outputs.iter().position(|ctx| ctx.wl_surface() == surface) else {
            return;
        };
        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        if ctx.scale == new_scale {
            return;
        }
        tracing::info!(output = %ctx.name, scale = new_scale, "output scale changed");
        ctx.scale = new_scale;
        ctx.update_physical_size();
        if ctx.configured {
            ctx.wl_surface()
                .set_buffer_scale(i32::try_from(new_scale).unwrap_or(1));
            self.configure_swapchain(index);
            let surface = surface.clone();
            self.draw(&surface);
        }
    }
}

impl CompositorHandler for PlatformState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.apply_scale(surface, u32::try_from(new_factor.max(1)).unwrap_or(1));
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if let Some(ctx) = self.outputs.iter_mut().find(|ctx| ctx.wl_surface() == surface) {
            ctx.frame_pending = false;
        }
        self.draw(surface);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for PlatformState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.add_output(qh, output);
    }

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let Some(info) = self.output_state.info(&output) else {
            return;
        };
        let new_scale = u32::try_from(info.scale_factor.max(1)).unwrap_or(1);
        if let Some(ctx) = self.outputs.iter().find(|ctx| ctx.wl_output == output) {
            let surface = ctx.wl_surface().clone();
            self.apply_scale(&surface, new_scale);
        }
    }

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let before = self.outputs.len();
        self.outputs.retain(|ctx| ctx.wl_output != output);
        if self.outputs.len() != before {
            tracing::info!("output removed; destroyed its layer surface");
            self.update_pointer_demand();
        }
    }
}

impl LayerShellHandler for PlatformState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.outputs.retain(|ctx| &ctx.layer != layer);
        tracing::warn!("compositor closed a layer surface");
        if self.outputs.is_empty() {
            tracing::info!("all outputs gone; idling until an output returns");
        }
        self.update_pointer_demand();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self.outputs.iter().position(|ctx| &ctx.layer == layer) else {
            return;
        };

        let (mut width, mut height) = configure.new_size;
        if width == 0 || height == 0 {
            let logical = self
                .outputs
                .get(index)
                .and_then(|ctx| self.output_state.info(&ctx.wl_output))
                .and_then(|info| info.logical_size);
            match logical {
                Some((w, h)) if w > 0 && h > 0 => {
                    width = u32::try_from(w).unwrap_or(1);
                    height = u32::try_from(h).unwrap_or(1);
                }
                _ => {
                    tracing::warn!("configure with zero size and no logical size; waiting");
                    return;
                }
            }
        }

        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        let first_configure = !ctx.configured;
        let previous_physical = ctx.physical_size;
        ctx.logical_size = (width, height);
        ctx.update_physical_size();
        ctx.wl_surface()
            .set_buffer_scale(i32::try_from(ctx.scale).unwrap_or(1));

        tracing::info!(
            output = %ctx.name,
            logical_width = width,
            logical_height = height,
            scale = ctx.scale,
            physical_width = ctx.physical_size.width,
            physical_height = ctx.physical_size.height,
            first_configure,
            "layer surface configured"
        );

        if first_configure || ctx.physical_size != previous_physical {
            self.configure_swapchain(index);
        }

        let surface = layer.wl_surface().clone();
        self.draw(&surface);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for PlatformState {
    fn event(
        state: &mut Self,
        _manager: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                state.toplevels.track(toplevel.id());
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                state.toplevels.finished();
                state.apply_pause();
            }
            _ => {}
        }
    }

    event_created_child!(PlatformState, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for PlatformState {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.toplevels.set_app_id(&id, app_id);
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: raw } => {
                state.toplevels.set_state(&id, &raw);
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output } => {
                state.toplevels.enter_output(&id, output);
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output } => {
                state.toplevels.leave_output(&id, &output);
            }
            zwlr_foreign_toplevel_handle_v1::Event::Done => state.apply_pause(),
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state.toplevels.forget(&id);
                handle.destroy();
                state.apply_pause();
            }
            _ => {}
        }
    }
}

impl ProvidesRegistryState for PlatformState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

impl SeatHandler for PlatformState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer {
            return;
        }
        let Ok(pointer) = self.seat_state.get_pointer(qh, &seat) else {
            return;
        };
        let device = self
            .cursor_shape
            .as_ref()
            .map(|mgr| mgr.get_pointer(&pointer, qh, ()));
        self.pointers.push((pointer, device));
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            for (pointer, device) in self.pointers.drain(..) {
                if let Some(d) = device {
                    d.destroy();
                }
                pointer.release();
            }
            self.buttons.set_left(false);
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for PlatformState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        const BTN_LEFT: u32 = 0x110;
        for event in events {
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = self
                        .pointers
                        .iter()
                        .find(|(p, _)| p == pointer)
                        .and_then(|(_, d)| d.as_ref())
                    {
                        device.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
                    }
                }
                PointerEventKind::Leave { .. } => self.buttons.set_left(false),
                PointerEventKind::Press { button: BTN_LEFT, .. } => self.buttons.set_left(true),
                PointerEventKind::Release { button: BTN_LEFT, .. } => self.buttons.set_left(false),
                _ => {}
            }
        }
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for PlatformState {
    fn event(
        _: &mut Self,
        _: &WpCursorShapeManagerV1,
        _: <WpCursorShapeManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for PlatformState {
    fn event(
        _: &mut Self,
        _: &WpCursorShapeDeviceV1,
        _: <WpCursorShapeDeviceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_seat!(PlatformState);
delegate_pointer!(PlatformState);
delegate_compositor!(PlatformState);
delegate_output!(PlatformState);
delegate_layer!(PlatformState);
delegate_registry!(PlatformState);
