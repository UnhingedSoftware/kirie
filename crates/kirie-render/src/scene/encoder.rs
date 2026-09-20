//! The command encoder for one frame, with the scene's render pass held open.
//!
//! A wallpaper composites its layers one after another into the same scene
//! buffer. Recorded a layer at a time, that is one render pass per layer per
//! pass per frame, each one drawing a single quad. Every one of those costs a
//! target bind, and on a tile-based GPU it costs a round trip of the whole
//! framebuffer out to memory and back — which is where a wallpaper's power
//! goes, since it draws forever in the background.
//!
//! So the scene's render pass is opened once and kept open: consecutive draws
//! into the scene land in it, and it closes only when something genuinely
//! needs it closed — a draw to another target, a texture copy, or the end of
//! the frame. The recorded draw order is unchanged, so the picture is too.

use crate::frame_cost;

/// Which attachment the open render pass is writing to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// The scene buffer every layer composites into.
    Scene,
    /// An object's offscreen buffer, or the surface itself. Never held open
    /// across calls, because these clear on load.
    Other,
}

pub struct FrameEncoder {
    encoder: wgpu::CommandEncoder,
    open: Option<(wgpu::RenderPass<'static>, Target)>,
    /// The scene buffer still holds the previous frame until this is set.
    cleared: bool,
    clear_color: wgpu::Color,
    /// Whether anything has been drawn into the scene since the snapshot that
    /// scene-reading shaders sample was last refreshed.
    dirty: bool,
    /// Set by `KIRIE_UNBATCHED_PASSES`: record the way the renderer used to,
    /// one render pass per draw and a snapshot copy every time one is asked
    /// for. It is here to measure what the batching saves, and to have
    /// something to fall back to if a driver ever dislikes the long pass.
    unbatched: bool,
}

static UNBATCHED: std::sync::OnceLock<std::sync::atomic::AtomicBool> = std::sync::OnceLock::new();

fn flag() -> &'static std::sync::atomic::AtomicBool {
    UNBATCHED.get_or_init(|| {
        std::sync::atomic::AtomicBool::new(std::env::var_os("KIRIE_UNBATCHED_PASSES").is_some())
    })
}

fn unbatched() -> bool {
    flag().load(std::sync::atomic::Ordering::Relaxed)
}

/// Turns pass batching off, as `KIRIE_UNBATCHED_PASSES` does. It is here so
/// the two ways of recording a frame can be compared in a test, and so a
/// driver that dislikes the long render pass has something to fall back to.
pub fn set_unbatched_passes(on: bool) {
    flag().store(on, std::sync::atomic::Ordering::Relaxed);
}

impl FrameEncoder {
    pub(crate) fn new(device: &wgpu::Device, clear_color: wgpu::Color) -> Self {
        Self {
            encoder: device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kirie-scene-encoder"),
            }),
            open: None,
            cleared: false,
            clear_color,
            dirty: false,
            unbatched: unbatched(),
        }
    }

    /// A render pass on the scene buffer, reusing the open one when there is
    /// one. The first call of the frame clears; the rest load.
    pub(crate) fn scene(&mut self, view: &wgpu::TextureView) -> &mut wgpu::RenderPass<'static> {
        self.dirty = true;
        if !self.unbatched && matches!(self.open, Some((_, Target::Scene))) {
            return &mut self.open.as_mut().expect("just matched").0;
        }
        let load = if self.cleared {
            wgpu::LoadOp::Load
        } else {
            self.cleared = true;
            wgpu::LoadOp::Clear(self.clear_color)
        };
        self.open(view, load, "kirie-scene", Target::Scene)
    }

    /// A render pass on a target that is not the scene buffer. Any open pass
    /// is closed first, and this one is not reused by a later call.
    pub(crate) fn offscreen(
        &mut self,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        label: &'static str,
    ) -> &mut wgpu::RenderPass<'static> {
        self.open(view, load, label, Target::Other)
    }

    fn open(
        &mut self,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        label: &'static str,
        target: Target,
    ) -> &mut wgpu::RenderPass<'static> {
        // Dropping the previous pass first is what unlocks the encoder.
        self.open = None;
        frame_cost::render_pass();
        let pass = self
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        self.open = Some((pass, target));
        &mut self.open.as_mut().expect("just set").0
    }

    /// A render pass with a depth attachment. Models need one; nothing is
    /// batched across it.
    pub(crate) fn with_depth(
        &mut self,
        view: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        label: &'static str,
    ) -> &mut wgpu::RenderPass<'static> {
        self.open = None;
        frame_cost::render_pass();
        let pass = self
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        self.open = Some((pass, Target::Other));
        self.dirty = true;
        &mut self.open.as_mut().expect("just set").0
    }

    /// The encoder itself, with no render pass open. Everything that records
    /// outside a render pass goes through here, which is what keeps the open
    /// pass from outliving its welcome.
    pub(crate) fn raw(&mut self) -> &mut wgpu::CommandEncoder {
        self.open = None;
        &mut self.encoder
    }

    /// Refreshes the snapshot that scene-reading shaders sample. Copying a
    /// full-screen `Rgba16Float` target is several megabytes of traffic each
    /// way, so it is skipped when nothing has been drawn since the last one.
    pub(crate) fn refresh_snapshot(
        &mut self,
        view: &wgpu::TextureView,
        scene: &wgpu::Texture,
        snapshot: &wgpu::Texture,
        extent: wgpu::Extent3d,
    ) {
        // What gets copied is the scene as it stands after the frame's clear,
        // which is the order the clear used to run in.
        self.ensure_cleared(view);
        if !self.dirty && !self.unbatched {
            frame_cost::snapshot_skipped();
            return;
        }
        frame_cost::texture_copy(super::renderer::fbo_bytes(extent));
        self.raw()
            .copy_texture_to_texture(scene.as_image_copy(), snapshot.as_image_copy(), extent);
        self.dirty = false;
    }

    /// The scene buffer holds the previous frame until something clears it, so
    /// a frame that draws nothing into it still has to.
    pub(crate) fn ensure_cleared(&mut self, view: &wgpu::TextureView) {
        if !self.cleared {
            self.scene(view);
        }
    }

    pub(crate) fn submit(mut self, queue: &wgpu::Queue) {
        self.open = None;
        queue.submit(Some(self.encoder.finish()));
    }
}
