use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};

use crate::audio::AudioLink;
use crate::clock::{WallClock, audio_position};
use crate::decode::DecodedFrame;
use crate::pacing::Pacer;
use crate::player::{RendererCmd, VideoPlayer};
use crate::scaling::{ScalingMode, UvRect, compute_uvs};

const STATS_INTERVAL: Duration = Duration::from_secs(2);

const SHADER: &str = r#"
struct Uniforms {
    rect: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var frame_tex: texture_2d<f32>;
@group(0) @binding(2) var frame_samp: sampler;

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
    let color = textureSample(frame_tex, frame_samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    return vec4<f32>(color.rgb * inside, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    rect: [f32; 4],
}

struct FrameTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
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
    uv_key: Option<(u32, u32, u32, u32, ScalingMode)>,

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

    fn ensure_texture(&mut self, width: u32, height: u32) {
        if self
            .frame_tex
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kirie-video-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        tracing::info!(width, height, "video frame texture (re)created");
        self.frame_tex = Some(FrameTexture {
            texture,
            bind_group,
            width,
            height,
        });
    }

    fn upload(&mut self, frame: DecodedFrame) {
        self.ensure_texture(frame.width, frame.height);
        let Some(tex) = &self.frame_tex else { return };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.data,
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
        let _ = self.recycle_tx.try_send(frame.data);
    }

    fn update_uvs(&mut self, size: SurfaceSize) {
        let Some(tex) = &self.frame_tex else { return };
        let key = (size.width, size.height, tex.width, tex.height, self.scaling);
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
