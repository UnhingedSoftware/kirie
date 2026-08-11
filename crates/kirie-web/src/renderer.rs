//! [`WebRenderer`] — uploads a [`WebBackend`]'s latest frame to a wgpu
//! texture and blits it fullscreen.
//!
//! Mirrors the C++ `CWeb::renderFrame` path (docs/subsystems-misc.md §3.5):
//! per frame, resize the texture if the browser's paint size changed, upload
//! the newest BGRA buffer, then draw. The heavy lifting (the actual browser
//! paint) happens on the backend's own thread; this renderer only ever reads
//! the last published frame, so it satisfies the frame-callback-driven,
//! non-blocking [`kirie_platform::Renderer`] contract (SPEC §V4/§V6).

use kirie_platform::{RenderTarget, Renderer, SurfaceSize};

use crate::backend::{PixelFormat, WebBackend, WebFrameRef};
use crate::feed::{FeedPump, WebFeed};

/// A [`kirie_platform::Renderer`] that presents a web wallpaper.
///
/// Owns the browser [`WebBackend`] and the GPU resources needed to sample its
/// off-screen frames onto the output surface.
pub struct WebRenderer {
    backend: Box<dyn WebBackend>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    bind_layout: wgpu::BindGroupLayout,
    /// Lazily (re)built when the browser paint size or format changes.
    uploaded: Option<Uploaded>,
    surface_size: SurfaceSize,
    /// Engine-supplied source of live audio + now-playing data
    /// ([`Self::set_feed`]); `None` leaves the page unfed, which is what a
    /// build with neither audio capture nor MPRIS wants.
    feed: Option<Box<dyn WebFeed>>,
    /// Rate limiting + change detection for that feed. Held here (not in the
    /// backend) so both drive paths — `render` for a composited backend,
    /// `poll` for a passive one — share one cadence and one diff.
    pump: FeedPump,
    /// Last platform pointer, surface-normalized, with the held button state —
    /// buttons arrive on a separate trait call, so both halves are retained
    /// and every forward carries the full state.
    pointer: (f32, f32),
    pointer_left: bool,
    /// Engine power-save flag + the last value forwarded to the backend, so
    /// the backend hears only transitions.
    power_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    power_on: bool,
}

struct Uploaded {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    format: PixelFormat,
}

impl WebRenderer {
    /// Push the retained pointer + button state to the page in browser pixels.
    fn forward_pointer(&mut self) {
        let px = (self.pointer.0 * self.surface_size.width as f32) as i32;
        let py = (self.pointer.1 * self.surface_size.height as f32) as i32;
        self.backend.send_pointer(crate::backend::PointerState {
            x: px,
            y: py,
            left: self.pointer_left,
            right: false,
        });
    }

    /// Build the fullscreen-blit pipeline for `target`, presenting `backend`.
    ///
    /// The pipeline targets the swapchain `format`; the browser texture is
    /// sampled through an sRGB view so web content (sRGB bytes) is linearised
    /// on read and re-encoded on write.
    #[must_use]
    pub fn new(target: &RenderTarget<'_>, backend: Box<dyn WebBackend>) -> Self {
        let device = target.device.clone();
        let queue = target.queue.clone();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kirie-web blit shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kirie-web bind layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kirie-web pipeline layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kirie-web blit pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kirie-web sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            backend,
            device,
            queue,
            pipeline,
            sampler,
            bind_layout,
            uploaded: None,
            surface_size: SurfaceSize { width: 1, height: 1 },
            feed: None,
            pump: FeedPump::new(),
            pointer: (0.5, 0.5),
            pointer_left: false,
            power_flag: None,
            power_on: false,
        }
    }

    /// Access the underlying backend (e.g. to forward pointer input).
    pub fn backend_mut(&mut self) -> &mut dyn WebBackend {
        self.backend.as_mut()
    }

    /// Attach the engine's live audio + now-playing source.
    ///
    /// Without this a web wallpaper renders, takes pointer input and receives
    /// its user properties, but its `wallpaperRegisterAudioListener` /
    /// `wallpaperRegisterMedia*Listener` callbacks never fire — an
    /// audio-reactive page stays still and a media page never leaves its
    /// loading state. The engine owns the sources (system-audio capture, the
    /// MPRIS D-Bus client), so it supplies them here rather than this crate
    /// growing a dependency on either.
    ///
    /// Delivery starts on the next [`Renderer::render`] / [`Renderer::poll`],
    /// with the first tick sending every media channel so the page is brought
    /// up to date even when nothing is playing.
    pub fn set_feed(&mut self, feed: Box<dyn WebFeed>) {
        self.feed = Some(feed);
    }

    /// Wire the engine's on-battery flag into the feed pump (halves the
    /// audio/media push cadence while set).
    pub fn set_power_save(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.pump.set_power_save(flag.clone());
        self.power_flag = Some(flag);
    }

    /// Forward a power-save transition to the backend (CEF drops its paint
    /// rate; native-surface backends ignore it).
    fn sync_power_save(&mut self) {
        let Some(flag) = &self.power_flag else { return };
        let on = flag.load(std::sync::atomic::Ordering::Relaxed);
        if on != self.power_on {
            self.power_on = on;
            self.backend.set_power_save(on);
        }
    }

    /// Deliver whatever the feed has that the page has not been told yet.
    ///
    /// Called from both drive paths; the pump itself decides what is due, so
    /// calling it more often than necessary costs two `Instant` comparisons.
    fn pump_feed(&mut self) {
        self.sync_power_save();
        if let Some(feed) = &self.feed {
            self.pump.pump(feed.as_ref(), self.backend.as_mut());
        }
    }

    fn ensure_texture(&mut self, frame: WebFrameRef<'_>) {
        let needs_new = match &self.uploaded {
            Some(u) => u.width != frame.width || u.height != frame.height || u.format != frame.format,
            None => true,
        };
        if needs_new {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("kirie-web frame"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: frame.format.wgpu_srgb(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kirie-web bind group"),
                layout: &self.bind_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.uploaded = Some(Uploaded {
                texture,
                bind_group,
                width: frame.width,
                height: frame.height,
                format: frame.format,
            });
        }
    }

    fn upload(&mut self, frame: WebFrameRef<'_>) {
        self.ensure_texture(frame);
        let Some(uploaded) = &self.uploaded else {
            return;
        };
        // Guard against a torn frame whose byte count disagrees with its
        // reported dims (SPEC §V9: never trust the buffer size).
        let expected = (frame.width as usize) * (frame.height as usize) * 4;
        if frame.data.len() < expected {
            return;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &uploaded.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.data[..expected],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
    }
}

impl Renderer for WebRenderer {
    fn is_passive(&self) -> bool {
        !self.backend.produces_frames()
    }

    fn render(&mut self, view: &wgpu::TextureView, size: SurfaceSize, dt: f32) {
        if size != self.surface_size {
            self.surface_size = size;
            self.backend.resize(crate::backend::WebSize {
                width: size.width,
                height: size.height,
            });
        }

        self.backend.tick(dt);
        // Composited backends (CEF) keep getting frames, so this is their feed
        // path; the pump's own intervals keep a 144 Hz output from pushing 144
        // audio frames a second. A passive backend never reaches here at all
        // and is fed from `poll` instead.
        self.pump_feed();

        // Copy the frame's bytes out from behind the backend's borrow before
        // touching `self` mutably (upload needs `&mut self`).
        if let Some(frame) = self.backend.latest_frame() {
            let owned = (frame.data.to_vec(), frame.width, frame.height, frame.format);
            self.upload(WebFrameRef {
                data: &owned.0,
                width: owned.1,
                height: owned.2,
                format: owned.3,
            });
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kirie-web encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-web blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(uploaded) = &self.uploaded {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &uploaded.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Platform-driven low-rate tick for the passive (webview) backend, whose
    /// [`Self::render`] is never called again after the first frame.
    ///
    /// Same work as the `render` path does inline: hand the page whatever the
    /// feed has that it has not been told yet. The platform only calls this for
    /// visible outputs, so a paused or released wallpaper is never fed — the
    /// host process it would push into is, in the released case, already gone.
    fn poll(&mut self) {
        self.pump_feed();
    }

    /// Platform-fed pointer (T26): forward to the page in browser pixels (the
    /// reference drives CEF mouse events the same way).
    fn set_pointer(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        self.forward_pointer();
    }

    /// Platform-fed button state: pages receive real mousedown/up (the
    /// backend edge-detects, so unchanged state costs one no-op line).
    fn set_pointer_buttons(&mut self, left_down: bool) {
        if self.pointer_left != left_down {
            self.pointer_left = left_down;
            self.forward_pointer();
        }
    }

    /// Hand the platform a still of the live page so it can leave it on the
    /// output when this wallpaper is released (`--release-hidden-after`).
    ///
    /// Delegated straight to the backend, because only it knows whether a still
    /// is obtainable *and* needed. A composited backend (CEF) answers `None`:
    /// the engine's surface already holds its pixels and keeps showing them
    /// after we stop drawing. The native-surface webview backend answers with a
    /// host-drawn frame, because its window disappears with the host and the
    /// engine's own surface underneath is black.
    fn snapshot(&mut self) -> Option<kirie_platform::RendererSnapshot> {
        let frame = self.backend.snapshot()?;
        Some(kirie_platform::RendererSnapshot {
            data: frame.data,
            width: frame.width,
            height: frame.height,
            format: match frame.format {
                PixelFormat::Bgra8 => kirie_platform::SnapshotFormat::Bgra8,
                PixelFormat::Rgba8 => kirie_platform::SnapshotFormat::Rgba8,
            },
        })
    }

    /// Live `setProperty` (doc §4.9): forward to the page as a one-entry
    /// `applyUserProperties` batch. Values are typed like the reference's
    /// encoder (`CWeb.cpp`): bools bare, numbers bare, everything else (colors
    /// are "r g b" strings there too) as a JSON string.
    fn set_property(&mut self, key: &str, value: &str) -> kirie_platform::PropertyImpact {
        let typed = match value.trim() {
            "true" => "true".to_owned(),
            "false" => "false".to_owned(),
            v if v.parse::<f64>().is_ok() => v.to_owned(),
            v => format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")),
        };
        let name = key.replace('\\', "\\\\").replace('"', "\\\"");
        let json = format!("{{\"{name}\":{{\"value\":{typed}}}}}");
        self.backend.apply_properties(&json);
        kirie_platform::PropertyImpact::Live
    }
}

impl Drop for WebRenderer {
    fn drop(&mut self) {
        self.backend.shutdown();
    }
}

/// Fullscreen-triangle blit. The oversized triangle covers the viewport; UVs
/// are derived from clip position with a top-left origin to match the
/// browser's top-left-origin paint buffer.
const BLIT_WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // (0,0) (2,0) (0,2) in UV space -> a triangle covering [0,1]^2.
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VsOut;
    out.uv = uv;
    out.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    // Clip-space y is up; texture/UV y is down. Flip so uv.y=0 is the top.
    out.pos.y = -out.pos.y;
    return out;
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(frame_tex, frame_sampler, in.uv);
}
"#;
