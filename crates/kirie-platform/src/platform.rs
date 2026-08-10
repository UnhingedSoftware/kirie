//! Wayland presentation driver: output enumeration + hotplug, one
//! layer-shell surface per output, frame-callback-driven rendering
//! (docs/render-architecture.md §2.3).

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

/// How often the pause watchdog re-derives every output's paused flag from the
/// tracked toplevel list while at least one output is paused.
///
/// Resuming is normally event-driven and immediate (a toplevel `state`/`closed`
/// event lands and `apply_pause` redraws), so this timer is not the mechanism —
/// it is the *backstop*. A paused output has deliberately torn down every one
/// of its own wake-up sources: no frame callback outstanding, no `--fps` timer
/// armed. That makes "we somehow never re-evaluate" the one failure mode that
/// would be permanent rather than self-correcting, e.g. a compositor that
/// updates a toplevel without the trailing `done` we key off. One cheap wakeup
/// a second while paused (a hash-map scan, no GPU work) bounds any such stall
/// to a second instead of forever. It costs nothing when nothing is paused —
/// the timer is only armed then, and drops itself as soon as the last output
/// resumes.
const PAUSE_WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);

/// How often [`Renderer::poll`] is driven for outputs whose renderer is passive.
///
/// A passive renderer (the webview web backend: webkit paints its own
/// layer-shell window over ours) gets exactly one `draw` after its first frame
/// and then nothing, ever — that early return is what keeps the event loop
/// alive (see the comment at the acquire in [`PlatformState::draw`]). It is
/// also, unavoidably, the end of every wake-up that renderer had. Web
/// wallpapers still need live data pushed into their page: the audio spectrum
/// an audio-reactive wallpaper animates from, and the MPRIS now-playing state a
/// media wallpaper displays. This timer is the replacement channel.
///
/// 30 Hz because that is the *audio* requirement — one 128-float JavaScript
/// call, ~1 KB, and the bands are already smoothed so half the reference's
/// per-frame cadence is visually identical. Media is far slower-moving and is
/// throttled again inside the renderer (`kirie_web::feed::MEDIA_INTERVAL`), so
/// nothing here is paced for it.
///
/// The cost when it does not apply is exactly zero: the timer is armed only
/// once a live, visible passive renderer exists
/// ([`PlatformState::ensure_passive_poll`]) and drops itself the moment none
/// does — including while an output is paused, so a fullscreen game does not
/// pay for a wallpaper it has covered.
const PASSIVE_POLL_INTERVAL: Duration = Duration::from_millis(33);

/// The wayland presentation layer: owns the compositor connection, all
/// per-output surfaces, and the shared GPU context (SPEC V1: everything
/// owned here, nothing global).
///
/// Selected by [`crate::Platform`] when a wayland session is present; the
/// X11 sibling is [`crate::x11::X11Platform`]. Both drive the same
/// [`crate::Renderer`] contract behind the shared output/surface model.
pub struct WaylandPlatform {
    event_loop: EventLoop<'static, PlatformState>,
    state: PlatformState,
    /// Clone-able sender for [`RenderCommand`]s applied on the render thread
    /// (live `bg` swap / preload); handed to the IPC applier.
    cmd_tx: CmdSender<RenderCommand>,
}

/// Handler state driven by the wayland event queue.
///
/// Field order is load-bearing for drop safety: `outputs` (which hold
/// `wgpu::Surface`s over raw libwayland pointers) and `gpu` are declared
/// before `conn`, so all surfaces are destroyed before the display
/// connection closes (see the SAFETY discussion in src/gpu.rs).
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
    /// Which layer to create surfaces on. The C++ driver defaults to
    /// `bottom` with the layer selectable via CLI
    /// (docs/render-architecture.md §2.3); kirie's presentation layer
    /// defaults to `background` and will expose the CLI selection in the
    /// compat layer (docs/compat-cli.md).
    layer: Layer,
    /// wlr-layer-shell surface namespace assigned to every surface. Must
    /// contain `wallpaperengine` so the daemon watchdog
    /// (`wallpaperengine.sh`, `engine_layer_ok()`:
    /// `any(.[][]?; .namespace|test("wallpaperengine"))`) recognises the
    /// live wallpaper and does not kill-restart the engine
    /// (see [`crate::PresentOptions`]).
    namespace: String,
    /// Output names (`--screen-root` values) that should get a wallpaper
    /// surface. Empty means every output. Any output not listed is left
    /// alone (no surface), so unconfigured monitors are not blacked out
    /// (SPEC V6: skipped outputs cost zero render work).
    screen_roots: Vec<String>,
    /// Minimum frame interval from `PresentOptions::fps` (`None` = uncapped).
    /// Live-updated by [`RenderCommand::SetFps`].
    min_frame: Option<std::time::Duration>,
    /// Playback-speed clock scale applied to every frame delta
    /// (`PresentOptions::playback_speed`; live-updated by
    /// [`RenderCommand::SetSpeed`]).
    playback_speed: f32,
    /// Set when the compositor closed the last layer surface — treated as
    /// abnormal, mirroring WaylandOpenGLDriver.cpp:234-274
    /// (docs/render-architecture.md §2.3).
    all_surfaces_closed: bool,
    /// Sender for the render thread's own command channel — build workers send
    /// `Install` back through it.
    cmd_tx: CmdSender<RenderCommand>,
    /// Preloaded renderers awaiting an instant [`RenderCommand::Swap`], keyed by
    /// (output name, preload key), stored with the format they were built for.
    preloaded: HashMap<(String, String), (wgpu::TextureFormat, Box<dyn crate::renderer::Renderer + Send>)>,
    /// Global cursor poller (T26; Hyprland IPC — inert elsewhere).
    pointer: crate::pointer::PointerPoll,
    /// Wayland seat plumbing for pointer button input (see
    /// [`crate::pointer::PointerButtons`] for why buttons come from the seat
    /// while position keeps the IPC poll: the poll sees the cursor everywhere,
    /// the seat only over the wallpaper — each is right for its half).
    seat_state: SeatState,
    /// cursor-shape-v1 manager, when the compositor offers it. Without an
    /// explicit shape the compositor may keep the previous app's cursor image
    /// while hovering the wallpaper.
    cursor_shape: Option<WpCursorShapeManagerV1>,
    /// Live pointers with their cursor-shape devices.
    pointers: Vec<(wl_pointer::WlPointer, Option<WpCursorShapeDeviceV1>)>,
    /// Shared button state the frame path reads.
    buttons: crate::pointer::PointerButtons,
    /// Supplies the off-thread launch-time build for an output (P1.6); `None`
    /// (or a `None` result) keeps that output on the synchronous factory.
    initial_build: Option<crate::renderer::InitialBuildFn>,
    /// Event-loop handle used to arm the `--fps` pacing timer. Sub-refresh
    /// pacing MUST be timer-driven, not a re-commit on every early frame
    /// callback: a bufferless commit forces the compositor to repaint the
    /// full screen, and re-committing per callback forms a feedback loop
    /// (commit → composite → callback → commit) that pins the GPU at the
    /// compositor's max composite rate regardless of `--fps`.
    loop_handle: LoopHandle<'static, PlatformState>,
    /// Mirror of every open toplevel plus the fullscreen-pause rule over it
    /// (src/toplevel.rs). Fed by the two `Dispatch` impls at the bottom of this
    /// file; read by [`PlatformState::apply_pause`].
    toplevels: ToplevelTracker,
    /// How long an output must stay hidden before its renderer is dropped to
    /// reclaim the memory (`PresentOptions::release_hidden_after`). `None`
    /// keeps hidden wallpapers resident.
    release_hidden_after: Option<Duration>,
    /// A [`PAUSE_WATCHDOG_INTERVAL`] timer is scheduled. Guards against
    /// stacking one watchdog per pause transition; cleared by the watchdog
    /// itself when it finds nothing paused and drops.
    pause_watchdog_armed: bool,
    /// A [`PASSIVE_POLL_INTERVAL`] timer is scheduled. Same guard shape as
    /// `pause_watchdog_armed`: one timer at a time, cleared by the timer itself
    /// when no visible passive renderer is left.
    passive_poll_armed: bool,
    /// The bound manager global, kept for the process lifetime so the
    /// compositor keeps streaming toplevel events at us. `None` when the
    /// compositor does not implement the protocol (GNOME) or when the pause was
    /// switched off with `--no-fullscreen-pause` — either way nothing pauses.
    _toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
}

impl WaylandPlatform {
    /// Connect to the wayland compositor named by `$WAYLAND_DISPLAY`, bind
    /// the required globals (`wl_compositor`, `zwlr_layer_shell_v1`,
    /// `wl_output`/`xdg_output`), and prepare the event loop.
    ///
    /// Output surfaces appear as `wl_output` globals are announced during
    /// [`Platform::run`] — the same path handles both initial enumeration
    /// and hotplug (docs/render-architecture.md §2.3: per requested output
    /// a viewport with its own surface is created).
    pub fn connect(make_renderer: RendererFactory) -> Result<Self, PlatformError> {
        Self::connect_with(make_renderer, crate::PresentOptions::default())
    }

    /// Connect with explicit [`crate::PresentOptions`] — the drop-in path.
    ///
    /// `options.layer_namespace` is stamped on every layer surface (the
    /// daemon watchdog greps it) and `options.screen_roots` restricts which
    /// outputs get a surface (empty = all). Both take effect for the initial
    /// enumeration and for any hotplugged output, since surface creation for
    /// every output flows through [`PlatformState::add_output`].
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
        // Optional: absent on compositors without cursor-shape-v1; hovering
        // then shows whatever cursor the compositor decides.
        let cursor_shape: Option<WpCursorShapeManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let registry_state = RegistryState::new(&globals);

        let event_loop = EventLoop::try_new()?;
        WaylandSource::new(conn.clone(), event_queue)
            .insert(event_loop.handle())
            .map_err(|err| PlatformError::EventLoopRegister(err.to_string()))?;

        // Command channel: another thread (the IPC applier) sends RenderCommands;
        // they are applied on THIS (render) thread between frames via the calloop
        // source callback — no lock, no surface sharing.
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

        // Fullscreen pause (docs/compat-cli.md §2 `--no-fullscreen-pause`):
        // bind the taskbar protocol so we can see when some *other* app goes
        // fullscreen on one of our outputs. Version 2 is the floor — that is
        // where `fullscreen` was added to the state enum, and without it the
        // feature has nothing to detect. Not binding when the pause is switched
        // off keeps an opted-out run completely off this protocol.
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
                // Not an error: GNOME/Mutter ships no foreign-toplevel
                // interface at all and never will, so this is the documented
                // "silently keep rendering" path rather than a broken session.
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
                min_frame: options
                    .fps
                    .filter(|f| *f > 0)
                    .map(|f| std::time::Duration::from_secs_f64(1.0 / f64::from(f))),
                playback_speed: if options.playback_speed > 0.0 {
                    options.playback_speed as f32
                } else {
                    1.0
                },
                all_surfaces_closed: false,
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

    /// Install the launch-time off-thread build supplier (P1.6). Call before
    /// [`Self::run`]; outputs it declines fall back to the sync factory.
    pub fn set_initial_build(&mut self, f: crate::renderer::InitialBuildFn) {
        self.state.initial_build = Some(f);
    }

    /// A clone-able sender for [`RenderCommand`]s. Hand this to the IPC applier so
    /// `bg`/`preload` build renderers off-thread and swap them in on the render
    /// thread (live switch, no relaunch).
    #[must_use]
    pub fn command_sender(&self) -> CmdSender<RenderCommand> {
        self.cmd_tx.clone()
    }

    /// Number of outputs that currently have a surface.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.state.outputs.len()
    }

    /// Dispatch compositor events — and therefore render frames — until
    /// `duration` elapses (`None` = run forever).
    ///
    /// Rendering happens exclusively from `wl_surface.frame` callbacks and
    /// configure events; between events this blocks in the event loop with
    /// zero CPU work (docs/render-architecture.md §2.3:
    /// `wl_display_dispatch` blocks when nothing needs redrawing; SPEC V6
    /// groundwork).
    pub fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        let deadline = duration.map(|d| Instant::now() + d);

        loop {
            if self.state.all_surfaces_closed {
                return Err(PlatformError::AllSurfacesClosed);
            }

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
    /// Apply a [`RenderCommand`] on the render thread (called from the calloop
    /// channel source, between frames — no lock, surface untouched here).
    fn handle_command(&mut self, cmd: RenderCommand) {
        match cmd {
            RenderCommand::Build { screen, stash, build } => {
                let Some(gpu) = &self.gpu else { return };
                let Some(ctx) = self.output_for(&screen) else {
                    return;
                };
                let Some(format) = ctx.format else { return }; // no frame drawn yet
                let name = ctx.name.clone();
                let size = (ctx.physical_size.width, ctx.physical_size.height);
                let device = gpu.device.clone();
                let queue = gpu.queue.clone();
                let tx = self.cmd_tx.clone();
                // Build off the render thread — the current wallpaper keeps
                // rendering. The worker sends the result back as `Install`.
                std::thread::spawn(move || {
                    let renderer = build(&device, &queue, format, &name, size);
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
                            // Cap the stash at one preloaded wallpaper per
                            // output: each entry holds a fully-built renderer
                            // (GPU textures + CPU state), and the disk caches
                            // (bundle + shaders) make a cold rebuild cheap —
                            // hoarding old builds is RAM/VRAM the compositor
                            // never sees again. Newest wins.
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
                let hit = self.preloaded.remove(&(name, key)).and_then(|(format, r)| {
                    // Only a format match is a real hit (surface may have
                    // reconfigured since the preload).
                    (self.outputs[idx].format == Some(format)).then_some(r)
                });
                match hit {
                    // Preload hit → instant pointer swap (sub-100ms).
                    Some(renderer) => {
                        tracing::info!(%screen, "preload hit — instant swap");
                        self.install_renderer(idx, renderer);
                    }
                    // Miss → build off-thread + install when ready (like Build).
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
                // Live property change: update the output's renderer in place and
                // repaint so it shows next frame (no reload). No-op if the output
                // or its renderer isn't up yet.
                let Some(idx) = self.output_index(&screen) else {
                    return;
                };
                let ctx = &mut self.outputs[idx];
                if let Some(renderer) = ctx.renderer.as_mut() {
                    // The flag starts `true` (assume structural) so a debounce
                    // that fires before this command was processed still
                    // rebuilds; an explicit Live verdict clears it.
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
                // Render-thread build (CEF web is !Send). Blocks the loop for the
                // build's duration — a brief hitch on the current wallpaper — then
                // installs. Needs the GPU + a drawn output (format known).
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
                let renderer = build_local(&device, &queue, format, &name, size);
                self.install_renderer(idx, renderer);
            }
            RenderCommand::SetFps(fps) => {
                self.min_frame = fps
                    .filter(|f| *f > 0)
                    .map(|f| std::time::Duration::from_secs_f64(1.0 / f64::from(f)));
            }
            RenderCommand::SetSpeed(speed) => {
                self.playback_speed = if speed > 0.0 { speed } else { 1.0 };
            }
            RenderCommand::Screenshot { screen, capture } => {
                // Capture the live frame on the render thread: the warm renderer
                // re-renders one frame to an offscreen texture (format matches the
                // surface, so its pipelines fit) and the app reads it back + writes
                // the file. Needs the GPU, a drawn output (format known) and a
                // renderer; any missing → drop (the daemon then falls back to the
                // workshop preview, which is why no error is surfaced here).
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

    /// Swap `outputs[idx]`'s renderer (the old one drops here) and request a
    /// repaint so the new wallpaper paints on the next frame. Takes a plain
    /// `Box<dyn Renderer>` (no `Send` bound) so it serves both the off-thread
    /// build (whose output is `Send`, coerced here) and the render-thread
    /// [`RenderCommand::SwapLocal`] build (whose output may be `!Send`, e.g. CEF).
    fn install_renderer(&mut self, idx: usize, renderer: Box<dyn crate::renderer::Renderer>) {
        let ctx = &mut self.outputs[idx];
        ctx.renderer = Some(renderer); // previous renderer drops here
        let was_initial = std::mem::take(&mut ctx.initial_build_pending); // launch build (P1.6) landed
        ctx.last_frame = None; // reset per-output dt for the fresh renderer
        let configured = ctx.configured;
        let surface = ctx.wl_surface().clone();
        // The old renderer just freed its CPU-side state (decoded assets,
        // script heaps, staging buffers) — return those pages to the kernel
        // now rather than letting glibc hoard them until the next build. GPU
        // resources already unmapped via the wgpu drop chain above. Burst-use
        // driver libraries (shader compiler etc.) get paged out too; they
        // refault at the next build.
        kirie_bake::trim_heap();
        kirie_bake::pageout_cold_libs();
        if let Some(gpu) = &self.gpu {
            crate::gpu::persist_pipeline_cache(&gpu.adapter);
        }
        tracing::info!(output = %self.outputs[idx].name, "wallpaper swapped in");

        // Paint the new wallpaper now rather than waiting for a frame callback.
        // Requesting a callback and committing without a buffer is not enough
        // for the FIRST frame: a compositor only repaints — and so only
        // dispatches the callback for — a surface that has a buffer, so an
        // output whose launch build ran off-thread (P1.6) would never present
        // and would stay black forever. Drawing here attaches that first
        // buffer; `draw` then re-arms the callback (or the `--fps` timer)
        // itself, so the chain is self-sustaining from this point. For a live
        // swap the output was already presenting, and drawing immediately just
        // makes the switch land a frame sooner.
        if configured {
            self.draw(&surface);
        } else if was_initial {
            // Not configured yet: the configure that follows kick-starts `draw`.
            tracing::debug!("launch build landed before configure; deferring first paint");
        }
    }

    /// The output matching `screen` (`"*"` = the first output, for window mode).
    fn output_for(&self, screen: &str) -> Option<&OutputContext> {
        if screen == "*" {
            self.outputs.first()
        } else {
            self.outputs.iter().find(|c| c.name == screen)
        }
    }

    /// Index of the output matching `screen` (`"*"` = the first).
    fn output_index(&self, screen: &str) -> Option<usize> {
        if screen == "*" {
            (!self.outputs.is_empty()).then_some(0)
        } else {
            self.outputs.iter().position(|c| c.name == screen)
        }
    }

    /// Create the layer surface + swapchain for a newly announced output
    /// (docs/render-architecture.md §2.3: per requested output one
    /// `wl_surface` + layer surface anchored to all four edges with
    /// exclusive zone -1; size is left at 0×0 so the compositor assigns
    /// the full output size via configure).
    fn add_output(&mut self, qh: &QueueHandle<Self>, wl_output: wl_output::WlOutput) {
        let info = self.output_state.info(&wl_output);
        let name = info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default();
        let scale = info
            .as_ref()
            .map(|i| u32::try_from(i.scale_factor.max(1)).unwrap_or(1))
            .unwrap_or(1);
        // Global logical position, for mapping the polled global cursor to
        // surface-local pointer coordinates (T26).
        let position = info.as_ref().map_or((0, 0), |i| (i.location.0, i.location.1));

        // Output selection: when a --screen-root list was supplied, only the
        // listed outputs get a surface. Every other output is left entirely
        // alone (no layer surface, no swapchain), so an unconfigured monitor
        // is never blacked out by a stray wallpaper surface. Empty list =
        // every output (matches the C++ engine's no-`--screen-root` default).
        // Applies to hotplugged outputs too, since they arrive here as well
        // (SPEC V6: a skipped output costs zero render work).
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
        // Initial commit (no buffer) requests the first configure.
        layer.wl_surface().commit();

        // Bring up the shared GPU context on the first surface; reuse the
        // instance for later outputs (docs/render-architecture.md §2.3
        // "wgpu:" note — shared device, per-monitor present pass).
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

        // A monitor hotplugged (or enumerated at startup) underneath an already
        // fullscreen app must come up paused, not render one wasted frame and
        // then wait for the next toplevel event to notice.
        self.apply_pause();
    }

    /// Recompute every output's paused flag from the tracked toplevels and act
    /// on the transitions.
    ///
    /// This is the *only* place `OutputContext::paused` changes, and it is
    /// deliberately a full recomputation rather than a delta: every caller
    /// (each toplevel event, output hotplug, the watchdog) hands it the same
    /// authoritative state, so the flag can never drift out of sync with the
    /// compositor's view no matter which event was missed or duplicated.
    ///
    /// Resuming is the half that must not be missed, so it is the half that
    /// does the work: an output that clears is redrawn *here*, for the same
    /// reason [`Self::install_renderer`] draws after an off-thread build. A
    /// paused output has no frame callback outstanding and no `--fps` timer
    /// armed — nothing else in the process would ever call `draw` for it again,
    /// and it would stay black under a window that is no longer there.
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
                    // `draw` below sees `renderer == None` and takes the same
                    // rebuild path as a cold start (off-thread when the app
                    // supplies an `InitialBuildFn`, else the sync factory).
                    ctx.released = false;
                    // Forget any frame callback that was still outstanding when
                    // this output paused. It was requested before we stopped
                    // committing, and whether the compositor still delivers it
                    // after a gap with no commits is not something to bet the
                    // resume on — with the flag clear, the `draw` below
                    // unconditionally requests a fresh one (uncapped mode; the
                    // `--fps` path arms its own timer regardless). Should the
                    // stale callback arrive too, it costs exactly one extra
                    // frame and then collapses back to one in flight.
                    ctx.frame_pending = false;
                    tracing::info!(output = %ctx.name, "wallpaper resumed");
                    resumed.push(ctx.wl_surface().clone());
                }
                // Still paused / still running: nothing to do. In particular do
                // NOT redraw a still-paused output, or the watchdog would turn
                // into the render loop it exists to stop.
                _ => {}
            }
        }

        // Restart the render chain for everything that just came back. `draw`
        // re-arms its own frame callback / `--fps` timer from here on, so one
        // call per output is enough to make it self-sustaining again.
        for surface in resumed {
            self.draw(&surface);
        }

        self.ensure_pause_watchdog();
    }

    /// Arm the pause watchdog if something is paused and it is not already
    /// running (see [`PAUSE_WATCHDOG_INTERVAL`] for why it exists).
    /// Drop the renderer of every output that has now been hidden longer than
    /// [`PresentOptions::release_hidden_after`](crate::PresentOptions).
    ///
    /// This is the whole point of having an explicit hidden signal. A covered
    /// wallpaper keeps its full footprint otherwise — scene textures and script
    /// heaps, and for a web wallpaper the out-of-process webkit host, which
    /// measurably frees nothing by itself while occluded. Dropping the renderer
    /// releases all of it (the web host dies with its backend), and the resume
    /// path rebuilds from the bake + shader caches.
    ///
    /// What the output *shows* afterwards needs no help for anything the engine
    /// composites itself: a wayland surface keeps its last committed buffer as
    /// long as the client stays silent (measured — a SIGSTOPped engine, which
    /// commits nothing whatsoever, still grabs its full frame seconds later), so
    /// a released scene/video/image goes on showing its final frame. The one
    /// exception is a **webview** web wallpaper, where webkit paints its own
    /// layer-shell window over ours and the engine's own last buffer is black:
    /// killing the host uncovers that black for the entire release. So before
    /// dropping such a renderer we ask it for a still of the live page and
    /// present that one frame ([`crate::snapshot::present_still`]) — nothing is
    /// retained, the compositor just keeps the buffer, and the wallpaper reads
    /// as frozen instead of gone.
    ///
    /// Called only from the pause watchdog, so it costs nothing while
    /// everything is visible.
    fn release_hidden_outputs(&mut self) {
        let Some(after) = self.release_hidden_after else {
            return;
        };
        // Cloned up front: the still needs the device/queue while `ctx` is
        // mutably borrowed below (cheap — wgpu handles are refcounted).
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
            // Ask the OUTGOING renderer for a still while it is still alive and
            // its host process still running. `None` for everything except the
            // webview backend, and `None` there too if the host is wedged (the
            // reply wait is bounded) — either way the release proceeds
            // unchanged. A stand-in is a bonus; it never gates the reclaim.
            let still = ctx.renderer.as_mut().and_then(|r| r.snapshot());
            // The renderer drops here: GPU resources unmap through wgpu's drop
            // chain, and a web backend kills its host process from `Drop`.
            ctx.renderer = None;
            ctx.released = true;
            // Present the still only after the reclaim has actually happened,
            // so a failure anywhere in the blit cannot cost the memory saving
            // it was only ever decorating. Ordering is otherwise free: the
            // output is hidden, so nothing is on screen to flicker.
            let stood_in = match (&still, &gpu, &ctx.wgpu_surface) {
                (Some(still), Some((device, queue)), Some(surface)) => {
                    crate::snapshot::present_still(device, queue, surface, still)
                }
                _ => false,
            };
            // Free the CPU-side copy before trimming, or the trim measures the
            // very buffer it is meant to reclaim.
            drop(still);
            // Same reclaim the swap path does — without the trim the freed
            // pages sit in glibc's arenas and the RSS never actually falls.
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
                // Re-derive from the tracked toplevels; this resumes (and
                // redraws) anything whose blocker is gone. `apply_pause` calls
                // back into `ensure_pause_watchdog`, which is a no-op while
                // `pause_watchdog_armed` is still set — so no second timer.
                state.apply_pause();
                // After `apply_pause`, so an output that just resumed is never
                // released on the same tick.
                state.release_hidden_outputs();
                if state.outputs.iter().any(|ctx| ctx.paused) {
                    TimeoutAction::ToDuration(PAUSE_WATCHDOG_INTERVAL)
                } else {
                    state.pause_watchdog_armed = false;
                    TimeoutAction::Drop
                }
            });
        if let Err(err) = armed {
            // Losing the backstop must not leave the flag lying: without it the
            // next transition could never arm one either. Event-driven resume
            // still works; only the safety net is gone.
            self.pause_watchdog_armed = false;
            tracing::warn!(%err, "could not arm the fullscreen-pause watchdog");
        }
    }

    /// Whether any output currently needs the passive poll: a live passive
    /// renderer on a **visible** output.
    ///
    /// The visibility half is not an optimisation. A paused output has had
    /// every one of its wake-up sources torn down on purpose, and a released
    /// one has no renderer at all; keeping a 30 Hz timer turning for either
    /// would hand back part of exactly what those features reclaim.
    fn needs_passive_poll(&self) -> bool {
        self.outputs
            .iter()
            .any(|ctx| !ctx.paused && !ctx.released && ctx.renderer.as_ref().is_some_and(|r| r.is_passive()))
    }

    /// Drive [`Renderer::poll`] on every visible output with a passive
    /// renderer.
    ///
    /// The same visibility filter as [`Self::needs_passive_poll`], applied
    /// again per tick rather than trusted from arming time: an output can pause
    /// between two ticks, and the whole point of the pause is that nothing
    /// touches it afterwards.
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

    /// Arm the passive-renderer poll if one is needed and no timer is running
    /// (see [`PASSIVE_POLL_INTERVAL`] for why it exists).
    ///
    /// Called from the one place `draw` can identify a passive renderer, which
    /// is also the place it stops rendering it — so the timer takes over
    /// exactly where the frame callbacks stop, and re-arms on resume because
    /// `apply_pause` redraws every output it clears.
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
                    // Nothing passive and visible any more (paused, released,
                    // or swapped for a scene): stop turning entirely. `draw`
                    // arms a fresh timer if one is ever needed again.
                    state.passive_poll_armed = false;
                    TimeoutAction::Drop
                }
            });
        if let Err(err) = armed {
            // Same discipline as the pause watchdog: never leave the flag set
            // on a failed arm, or nothing could ever arm one again.
            self.passive_poll_armed = false;
            tracing::warn!(%err, "could not arm the passive-renderer poll; web pages get no live data");
        }
    }

    /// (Re)configure the swapchain of `outputs[index]` for its current
    /// physical size and mark the full surface opaque, as the C++ driver
    /// does (docs/render-architecture.md §2.3: opaque region
    /// full-surface).
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
        // Fifo is universally supported and compositor-paced, matching the
        // frame-callback driven model (docs/render-architecture.md §2.3).
        config.present_mode = wgpu::PresentMode::Fifo;
        // Record the surface format now, not on the first drawn frame: the
        // off-thread build path (`RenderCommand::Build`, and the P1.6 launch
        // build) needs it to compile pipelines, so learning it only after a
        // frame had been drawn meant a `bg`/`preload` arriving before the
        // first frame was silently dropped — and the launch build could not
        // start off-thread at all.
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

    /// Render one frame for the output backing `surface` and present it.
    ///
    /// Called from exactly two places, mirroring the C++ driver: the first
    /// configure (kick-start, WaylandOpenGLDriver.cpp:405-440) and each
    /// `wl_surface.frame` callback (WaylandOutputViewport.cpp:94-105) —
    /// never from a busy loop (docs/render-architecture.md §2.3).
    fn draw(&mut self, surface: &wl_surface::WlSurface) {
        let Some(index) = self.outputs.iter().position(|ctx| ctx.wl_surface() == surface) else {
            return;
        };

        // Split borrows: take what we need without holding &mut self.
        let Some(gpu) = &self.gpu else { return };
        let (device, queue) = (gpu.device.clone(), gpu.queue.clone());

        let Some(ctx) = self.outputs.get_mut(index) else {
            return;
        };
        if !ctx.configured {
            return;
        }

        // Fullscreen pause: a game is covering this output, so do nothing —
        // and, unlike the acquire-failure path further down, do NOT re-arm the
        // frame callback or the `--fps` timer. That is the whole point: the
        // reference engine stops rendering under a fullscreen app so the app
        // gets the GPU, and a "paused" loop that still committed once a frame
        // would keep the compositor recompositing and give most of it back.
        // Placed ahead of the launch-build dispatch as well, so an engine
        // started while a game is already running does not spend a CPU core
        // building a wallpaper nobody can see; the build runs on resume.
        //
        // This return is what makes resuming a correctness requirement rather
        // than a nicety: every wake-up source for this output is now gone, and
        // only `apply_pause` can bring it back.
        if ctx.paused {
            return;
        }

        // `--fps` pacing MUST run before the swapchain acquire: acquiring
        // first and early-returning drops the SurfaceTexture without
        // presenting it — Vulkan has no un-acquire, so a monitor whose frame
        // callbacks arrive faster than the cap (144Hz vs --fps 123) exhausts
        // the swapchain within ~3 frames; every later acquire times out and
        // the output freezes (the live-desktop freeze this guards).
        //
        // When too early, pace with a one-shot calloop timer — do NOT
        // re-request the frame callback and commit here. A bufferless commit
        // makes the compositor repaint the whole screen, and re-committing on
        // every early callback forms a feedback loop (commit → composite →
        // callback → commit) that runs at the compositor's max composite rate
        // (e.g. ~90 fps on a weak iGPU) no matter how low `--fps` is set,
        // pinning the GPU. The timer wakes `draw` at the target time with no
        // intervening commits, so the compositor only repaints when we
        // actually present a new frame — GPU cost then scales with `--fps`.
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

        // P1.6: build this output's launch-time wallpaper on a worker instead
        // of inside `draw`. Must happen BEFORE the swapchain acquire — an
        // early return after acquiring drops the `SurfaceTexture` without
        // presenting it, and Vulkan has no un-acquire (see the pacing note
        // above). Nothing is presented until the `Install` lands, which is
        // what the synchronous path did too: it simply blocked here instead,
        // freezing the event loop (no IPC, no configure) and serializing the
        // builds of every other output.
        if self.outputs[index].renderer.is_none() {
            if self.outputs[index].initial_build_pending {
                return; // worker in flight; `install_renderer` re-arms the callback
            }
            let name = self.outputs[index].name.clone();
            // Only dispatch when the format is known (set at configure time) —
            // `RenderCommand::Build` drops the request otherwise, which would
            // strand this output with `initial_build_pending` set forever.
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
            // Declined (e.g. a `!Send` web backend) → fall through to the
            // synchronous factory below.
        }

        // A passive renderer (webview: webkit paints its own window over ours)
        // has nothing left to present once its first buffer is attached, and
        // acquiring again would block forever because the compositor never
        // composites a fully covered surface. Bow out BEFORE the acquire and
        // request no callback: the output is done, and the event loop stays
        // free to serve IPC, playlists, hotplug and fullscreen-pause.
        //
        // This is also the last moment this renderer is reachable from the
        // frame path, so it is where the substitute wake-up is armed: the
        // passive poll takes over feeding the page (audio + MPRIS) from here.
        // Arming on resume is covered too — `apply_pause` redraws every output
        // it un-pauses, which lands right back on this branch.
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

        // Outdated/Lost: reconfigure once and retry (wgpu 30 contract on
        // `CurrentSurfaceTexture`).
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
            // Acquire failed (Timeout/Occluded, or Outdated/Lost that
            // survived one reconfigure). This is a swapchain stall, not
            // compositor throttling: the frame callback for this round has
            // already fired (`frame_pending` was cleared), so returning
            // without re-arming would freeze this output forever — no
            // event would ever call `draw` again. Re-request the callback
            // and commit (no buffer attach) so the chain stays alive; this
            // still satisfies SPEC V6 because an occluded/DPMS-off output
            // simply never gets the callback delivered, costing zero work.
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
            })
        });

        // Per-output dt, seconds; 0 on the first frame
        // (docs/render-architecture.md §2.1 step 3, §2.3 per-output
        // cadence). Scaled by the playback-speed clock — the reference scales
        // g_Time the same way (WallpaperApplication.cpp:908).
        let now = Instant::now();
        let dt = ctx
            .last_frame
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.0)
            * self.playback_speed;
        ctx.last_frame = Some(now);

        // Pointer (T26): map the polled global cursor into this surface's
        // normalized [0,1] coords (top-left origin). Unknown cursor / zero
        // size ⇒ don't call — the renderer keeps its centered default.
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

        // Uncapped: request the next frame callback *before* presenting so it
        // rides the same commit and vsync-paces the loop (the C++ driver's
        // swapOutput does the same, WaylandOutputViewport.cpp:263-273).
        //
        // Capped (`--fps`): do NOT request a frame callback — drive the next
        // frame from a timer (armed at the end of this fn). An outstanding
        // wallpaper frame callback makes the compositor schedule a repaint on
        // every monitor refresh to dispatch it, so it recomposites the whole
        // screen at the full refresh rate (e.g. 360Hz) and pins the GPU even
        // when we only present a few frames a second. A static wallpaper —
        // which never requests a callback — leaves the compositor idle; the
        // timer path makes a capped live wallpaper behave the same way.
        if self.min_frame.is_none() && !ctx.frame_pending {
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
            // Initial build settled: drop the build-burst pages (glibc arenas
            // + shader-compiler/raytracing driver libs) from RSS and persist
            // the driver pipeline cache. Swaps do the same in install_renderer.
            kirie_bake::trim_heap();
            kirie_bake::pageout_cold_libs();
            if let Some(gpu) = &self.gpu {
                crate::gpu::persist_pipeline_cache(&gpu.adapter);
            }
        }

        // Capped: schedule the next render off a one-shot timer instead of a
        // frame callback (see the present block above). One timer in flight at
        // a time — `timer_armed` also covers a timer armed by the early-callback
        // skip path, so the two never stack.
        if let Some(min) = self.min_frame {
            let ctx = &mut self.outputs[index];
            if !ctx.timer_armed {
                ctx.timer_armed = true;
                let surface = ctx.wl_surface().clone();
                let timer = Timer::from_duration(min);
                let _ = self.loop_handle.insert_source(timer, move |_, _, state| {
                    if let Some(ctx) = state.outputs.iter_mut().find(|c| c.wl_surface() == &surface) {
                        ctx.timer_armed = false;
                    }
                    state.draw(&surface);
                    TimeoutAction::Drop
                });
            }
        }
    }

    /// Apply a scale change to the output backing `surface`
    /// (docs/render-architecture.md §2.3: buffer scale is re-asserted per
    /// frame in C++; here we reconfigure when it actually changes).
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
        // Buffer transform optimization not implemented; the compositor
        // handles rotation of untransformed buffers.
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // The compositor wants a new frame for this output
        // (docs/render-architecture.md §2.3: frame callback fires →
        // render this viewport again).
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
        // Hotplug removal: drop the whole per-output context (swapchain
        // first, then layer surface — field order in OutputContext).
        let before = self.outputs.len();
        self.outputs.retain(|ctx| ctx.wl_output != output);
        if self.outputs.len() != before {
            tracing::info!("output removed; destroyed its layer surface");
        }
    }
}

impl LayerShellHandler for PlatformState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.outputs.retain(|ctx| &ctx.layer != layer);
        tracing::warn!("compositor closed a layer surface");
        if self.outputs.is_empty() {
            // Abnormal: supervisor should relaunch
            // (docs/render-architecture.md §2.3,
            // WaylandOpenGLDriver.cpp:234-274).
            self.all_surfaces_closed = true;
        }
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

        // Compositor-suggested size in surface-local (logical)
        // coordinates. Anchored to all four edges with exclusive zone -1
        // this is the full output size (docs/render-architecture.md §2.3).
        // A zero dimension means "pick your own"; fall back to the
        // output's logical size, else skip until a real size arrives.
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
        // Integer buffer scale, as the C++ driver sets per swap
        // (docs/render-architecture.md §2.3:
        // `wl_surface_set_buffer_scale(scale)`).
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

        // Only rebuild the swapchain when the size actually changed (or on
        // the first / a previously failed configure): wgpu's
        // `Surface::configure` waits for the GPU to go idle before
        // recreating the swapchain, so doing it on a spurious same-size
        // configure would stall the pipeline for nothing.
        if first_configure || ctx.physical_size != previous_physical {
            self.configure_swapchain(index);
        }

        // Kick-start rendering: the first frame is drawn from the
        // configure, after which frame callbacks take over
        // (docs/render-architecture.md §2.3,
        // WaylandOpenGLDriver.cpp:405-440). Redraws on later configures
        // pick up the new size immediately.
        let surface = layer.wl_surface().clone();
        self.draw(&surface);
    }
}

/// Manager side of the fullscreen pause: one `toplevel` event per open window,
/// then `finished` if the compositor tears the manager down.
///
/// Hand-written rather than delegated — smithay-client-toolkit has no
/// foreign-toplevel module, so this crate owns the (small) protocol mirror in
/// src/toplevel.rs.
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
                // Details (app_id, state, outputs) arrive as separate events on
                // the handle and are terminated by `done`; start with an empty
                // entry so those setters have somewhere to land.
                state.toplevels.track(toplevel.id());
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                // The manager is dead and no further toplevel event can ever
                // arrive. Anything paused right now would be paused on frozen
                // information, so drop the mirror and resume everything.
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

/// Per-window side of the fullscreen pause.
///
/// The protocol batches property changes and terminates each batch with `done`,
/// so the pause decision is re-derived there (and on `closed`) rather than on
/// every individual event — otherwise a freshly mapped fullscreen window would
/// briefly pause under its empty pre-`app_id` identity before the ignore list
/// could be consulted.
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
                // No `done` follows `closed`, and this is the event that ends
                // the pause in the ordinary case (the game exited), so it
                // re-derives on its own. `destroy` finalizes the now-inert
                // object, as the protocol asks.
                state.toplevels.forget(&id);
                handle.destroy();
                state.apply_pause();
            }
            // Title / parent changes cannot affect the pause rule.
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
        /// Linux evdev BTN_LEFT.
        const BTN_LEFT: u32 = 0x110;
        for event in events {
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    // The wallpaper never wants a special cursor; an explicit
                    // Default keeps the compositor from freezing the previous
                    // client's image while over the desktop.
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
