use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};

use crate::audio::AudioLink;
use crate::clock::{WallClock, audio_position};
use crate::decode::{DecodedFrame, FramePixels};
use crate::pacing::Pacer;
use crate::player::{RendererCmd, VideoPlayer};
use crate::scaling::{ScalingMode, UvRect, compute_uvs};

const STATS_INTERVAL: Duration = Duration::from_secs(2);

const SHADER: &str = r#"
struct Uniforms {
    rect: vec4<f32>,
    // x: 1 when the frame is NV12 (frame_tex is luma, chroma_tex is CbCr),
    // y: 1 for BT.601 rather than BT.709, z: 1 for full-range YUV,
    // w: 1 when the target is sRGB and the result has to be linearised.
    yuv: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var frame_tex: texture_2d<f32>;
@group(0) @binding(2) var frame_samp: sampler;
@group(0) @binding(3) var chroma_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Full-viewport triangle strip; uv is screen-space 0..1, y down.
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[index];
    var out: VsOut;
    out.pos = vec4<f32>(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0);
    out.uv = c;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = mix(u.rect.xy, u.rect.zw, in.uv);
    let inside = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0);
    let at = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
    var color = textureSample(frame_tex, frame_samp, at).rgb;
    let chroma = textureSample(chroma_tex, frame_samp, at).rg - vec2<f32>(0.5);
    if (u.yuv.x > 0.5) {
        var y = color.r;
        var c = chroma;
        if (u.yuv.z < 0.5) {
            y = (y - 16.0 / 255.0) * (255.0 / 219.0);
            c = c * (255.0 / 224.0);
        }
        if (u.yuv.y > 0.5) {
            color = vec3<f32>(y + 1.402 * c.y, y - 0.344136 * c.x - 0.714136 * c.y, y + 1.772 * c.x);
        } else {
            color = vec3<f32>(y + 1.5748 * c.y, y - 0.187324 * c.x - 0.468124 * c.y, y + 1.8556 * c.x);
        }
        color = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
        if (u.yuv.w > 0.5) {
            color = select(
                pow((color + 0.055) / 1.055, vec3<f32>(2.4)),
                color / 12.92,
                color <= vec3<f32>(0.04045),
            );
        }
    }
    return vec4<f32>(color * inside, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    rect: [f32; 4],
    yuv: [f32; 4],
}

/// What the frame textures hold: RGBA in `main`, or NV12's luma in `main`
/// and its half-size CbCr plane in `chroma`. RGBA binds a 1x1 stand-in as
/// `chroma`, which the shader never reads from.
struct FrameTexture {
    main: wgpu::Texture,
    chroma: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    pixels: FramePixels,
}

pub struct VideoRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    texture_format: wgpu::TextureFormat,
    frame_tex: Option<FrameTexture>,
    uv_key: Option<(u32, u32, u32, u32, ScalingMode, [u32; 4])>,
    yuv: [f32; 4],

    frames_rx: Receiver<DecodedFrame>,
    recycle_tx: Sender<Vec<u8>>,
    commands_rx: Receiver<RendererCmd>,
    audio: Option<AudioLink>,
    wall: WallClock,
    pacer: Pacer<DecodedFrame>,
    scaling: ScalingMode,
    last_pos: f64,

    stats_anchor: Instant,
    stats_presented: u64,
    stats_dropped: u64,
    _shutdown: Sender<()>,
}

impl VideoRenderer {
    #[must_use]
    pub fn new(target: &RenderTarget<'_>, player: VideoPlayer) -> Self {
        let device = target.device;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kirie-video-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kirie-video-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kirie-video-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kirie-video-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kirie-video-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kirie-video-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let texture_format = if target.format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };

        let now = Instant::now();
        let parts = player.into_parts();
        Self {
            device: target.device.clone(),
            queue: target.queue.clone(),
            pipeline,
            bind_group_layout,
            sampler,
            uniforms,
            texture_format,
            frame_tex: None,
            uv_key: None,
            yuv: [0.0; 4],
            frames_rx: parts.frames_rx,
            recycle_tx: parts.recycle_tx,
            commands_rx: parts.commands_rx,
            audio: parts.audio,
            wall: WallClock::new(now, parts.paused),
            pacer: Pacer::new(),
            scaling: parts.scaling,
            last_pos: 0.0,
            stats_anchor: now,
            stats_presented: 0,
            stats_dropped: 0,
            _shutdown: parts.shutdown,
        }
    }

    fn clock_now(&mut self, now: Instant) -> f64 {
        match self.audio.as_mut() {
            Some(link) => {
                let prod = *link.producer.read();
                let cons = *link.consumer.read();
                let pos = audio_position(&prod, &cons, link.sample_rate, now);
                self.last_pos = self.last_pos.max(pos);
                self.last_pos
            }
            None => self.wall.now(now),
        }
    }

    fn drain_commands(&mut self, now: Instant) {
        while let Ok(cmd) = self.commands_rx.try_recv() {
            match cmd {
                RendererCmd::Pause(paused) => self.wall.set_paused(paused, now),
                RendererCmd::Speed(speed) => self.wall.set_speed(speed, now),
                RendererCmd::Scaling(mode) => self.scaling = mode,
            }
        }
    }

    fn plane(&self, label: &str, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn ensure_texture(&mut self, width: u32, height: u32, pixels: FramePixels) {
        if self
            .frame_tex
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height && t.pixels == pixels)
        {
            return;
        }
        let (main, chroma) = match pixels {
            FramePixels::Rgba => (
                self.plane("kirie-video-frame", width, height, self.texture_format),
                self.plane("kirie-video-no-chroma", 1, 1, wgpu::TextureFormat::Rg8Unorm),
            ),
            FramePixels::Nv12 => (
                self.plane("kirie-video-luma", width, height, wgpu::TextureFormat::R8Unorm),
                self.plane(
                    "kirie-video-chroma",
                    width / 2,
                    height / 2,
                    wgpu::TextureFormat::Rg8Unorm,
                ),
            ),
        };
        let main_view = main.create_view(&wgpu::TextureViewDescriptor::default());
        let chroma_view = chroma.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-video-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&main_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&chroma_view),
                },
            ],
        });
        tracing::info!(width, height, ?pixels, "video frame texture (re)created");
        self.frame_tex = Some(FrameTexture {
            main,
            chroma,
            bind_group,
            width,
            height,
            pixels,
        });
    }

    fn write_plane(
        &self,
        texture: &wgpu::Texture,
        data: &[u8],
        width: u32,
        height: u32,
        bytes_per_texel: u32,
    ) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * bytes_per_texel),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn upload(&mut self, frame: DecodedFrame) {
        let (width, height) = (frame.width, frame.height);
        // A texture the device cannot hold is a wgpu validation error, which
        // panics, and the size comes from the file.
        let max = self.device.limits().max_texture_dimension_2d;
        let pixels = width as usize * height as usize;
        let expected = match frame.pixels {
            FramePixels::Rgba => pixels * 4,
            FramePixels::Nv12 => pixels + pixels / 2,
        };
        if width > max || height > max || frame.data.len() < expected {
            let _ = self.recycle_tx.try_send(frame.data);
            return;
        }
        self.ensure_texture(width, height, frame.pixels);
        let Some(tex) = &self.frame_tex else { return };
        match frame.pixels {
            FramePixels::Rgba => {
                self.write_plane(&tex.main, &frame.data, width, height, 4);
                self.yuv = [0.0; 4];
            }
            FramePixels::Nv12 => {
                let (y, cbcr) = frame.data.split_at(pixels);
                self.write_plane(&tex.main, y, width, height, 1);
                self.write_plane(&tex.chroma, cbcr, width / 2, height / 2, 2);
                let flag = |on: bool| if on { 1.0 } else { 0.0 };
                self.yuv = [
                    1.0,
                    flag(frame.bt601),
                    flag(frame.full_range),
                    flag(self.texture_format.is_srgb()),
                ];
            }
        }
        let _ = self.recycle_tx.try_send(frame.data);
    }

    fn update_uvs(&mut self, size: SurfaceSize) {
        let Some(tex) = &self.frame_tex else { return };
        let key = (
            size.width,
            size.height,
            tex.width,
            tex.height,
            self.scaling,
            self.yuv.map(f32::to_bits),
        );
        if self.uv_key == Some(key) {
            return;
        }
        self.uv_key = Some(key);
        let UvRect {
            ustart,
            uend,
            vstart,
            vend,
        } = compute_uvs(self.scaling, size.width, size.height, tex.width, tex.height);
        self.queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&Uniforms {
                rect: [ustart, vstart, uend, vend],
                yuv: self.yuv,
            }),
        );
    }

    fn maybe_log_stats(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.stats_anchor);
        if elapsed < STATS_INTERVAL {
            return;
        }
        let stats = self.pacer.stats();
        let presented = stats.presented - self.stats_presented.min(stats.presented);
        let dropped = stats.dropped - self.stats_dropped.min(stats.dropped);
        let fps = presented as f64 / elapsed.as_secs_f64();
        tracing::info!(
            fps = format!("{fps:.1}"),
            presented,
            dropped,
            clock = if self.audio.is_some() { "audio" } else { "wall" },
            "video playback"
        );
        self.stats_anchor = now;
        self.stats_presented = stats.presented;
        self.stats_dropped = stats.dropped;
    }
}

impl Renderer for VideoRenderer {
    fn render(&mut self, view: &wgpu::TextureView, size: SurfaceSize, _dt: f32) {
        let now = Instant::now();
        self.drain_commands(now);

        let media_now = self.clock_now(now);
        let due = self.pacer.select(
            media_now,
            || self.frames_rx.try_recv().ok(),
            |late| {
                let _ = self.recycle_tx.try_send(late.data);
            },
        );
        if let Some(frame) = due {
            self.upload(frame);
        }
        self.update_uvs(size);
        self.maybe_log_stats(now);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kirie-video-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-video-pass"),
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
            if let Some(tex) = &self.frame_tex {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &tex.bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
    }
}
