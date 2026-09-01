use std::collections::HashMap;

use kirie_scene::resolve::AssetSource;

use crate::content::{FramePlacement, ImageContent, ImagePage};
use crate::error::RenderError;
use crate::schedule::FrameSchedule;

#[derive(Debug)]
pub struct GpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub width: u32,
    pub height: u32,
    pub uv_crop: [f32; 2],
    pub real_size: [f32; 2],
}

type TextureCell = std::sync::Arc<std::sync::OnceLock<Option<std::sync::Arc<GpuTexture>>>>;

pub struct Nv12Rig {
    y: wgpu::Texture,
    uv: wgpu::Texture,
    bind: wgpu::BindGroup,
    pipeline: std::sync::Arc<wgpu::RenderPipeline>,
    target_view: wgpu::TextureView,
}

const NV12_WGSL: &str = r#"
@group(0) @binding(0) var yt: texture_2d<f32>;
@group(0) @binding(1) var uvt: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct VOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f }
@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
  var p = array<vec2f, 3>(vec2f(-1.0, -3.0), vec2f(-1.0, 1.0), vec2f(3.0, 1.0));
  var o: VOut;
  let q = p[i];
  o.pos = vec4f(q, 0.0, 1.0);
  o.uv = vec2f((q.x + 1.0) * 0.5, 1.0 - (q.y + 1.0) * 0.5);
  return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4f {
  let y = textureSample(yt, s, in.uv).r;
  let uv = textureSample(uvt, s, in.uv).rg - vec2f(0.5, 0.5);
  let yl = (y - 16.0 / 255.0) * (255.0 / 219.0);
  let u = uv.x * (255.0 / 224.0);
  let v = uv.y * (255.0 / 224.0);
  let rgb = vec3f(yl + 1.5748 * v, yl - 0.1873 * u - 0.4681 * v, yl + 1.8556 * u);
  return vec4f(clamp(rgb, vec3f(0.0), vec3f(1.0)), 1.0);
}
"#;

impl Nv12Rig {
    fn new(
        device: &wgpu::Device,
        pipeline: std::sync::Arc<wgpu::RenderPipeline>,
        target: &GpuTexture,
        w: u32,
        h: u32,
    ) -> Self {
        let plane = |width: u32, height: u32, format: wgpu::TextureFormat, label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
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
        };
        let y = plane(w, h, wgpu::TextureFormat::R8Unorm, "kirie-nv12-y");
        let uv = plane(w / 2, h / 2, wgpu::TextureFormat::Rg8Unorm, "kirie-nv12-uv");
        let y_view = y.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-nv12-bind"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let target_view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            y,
            uv,
            bind,
            pipeline,
            target_view,
        }
    }

    pub fn convert(&self, device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, data: &[u8]) {
        let y_bytes = (w * h) as usize;
        let uv_bytes = (w * (h / 2)) as usize;
        if data.len() < y_bytes + uv_bytes {
            return;
        }
        let write = |tex: &wgpu::Texture, bytes: &[u8], row: u32, height: u32| {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width: tex.width(),
                    height,
                    depth_or_array_layers: 1,
                },
            );
        };
        write(&self.y, &data[..y_bytes], w, h);
        write(&self.uv, &data[y_bytes..y_bytes + uv_bytes], w, h / 2);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kirie-nv12-convert"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-nv12-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.target_view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

#[allow(clippy::too_many_arguments)]
fn upload_video_target(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    name: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    nearest: bool,
    clamp: bool,
) -> GpuTexture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(name),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let filter = if nearest {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    let address = if clamp {
        wgpu::AddressMode::ClampToEdge
    } else {
        wgpu::AddressMode::Repeat
    };
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: filter,
        min_filter: filter,
        address_mode_u: address,
        address_mode_v: address,
        ..Default::default()
    });
    GpuTexture {
        texture,
        view,
        sampler,
        width,
        height,
        real_size: [width as f32, height as f32],
        uv_crop: [1.0, 1.0],
    }
}

fn nv12_pipeline(device: &wgpu::Device) -> std::sync::Arc<wgpu::RenderPipeline> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-nv12"),
        source: wgpu::ShaderSource::Wgsl(NV12_WGSL.into()),
    });
    std::sync::Arc::new(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-nv12"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    }))
}

pub struct VideoTexture {
    pub name: String,
    pub player: kirie_video::VideoPlayer,
    pub control: kirie_video::VideoControl,
    pub paused: std::cell::Cell<bool>,
    pub script_paused: std::cell::Cell<bool>,
    pub gpu: std::sync::Arc<GpuTexture>,
    pub nv12: Option<Nv12Rig>,
    pub size: (u32, u32),
}

pub struct AtlasTexture {
    pub frames: Vec<FramePlacement>,
    pub schedule: FrameSchedule,
    pub pages: Vec<ImagePage>,
    pub gpu: std::sync::Arc<GpuTexture>,
}

impl AtlasTexture {
    #[must_use]
    pub fn placement_at(&self, elapsed: f64) -> &FramePlacement {
        let index = self.schedule.frame_at(elapsed).min(self.frames.len() - 1);
        &self.frames[index]
    }
}

pub struct TextureRegistry {
    device: wgpu::Device,
    queue: wgpu::Queue,
    cache: std::sync::Mutex<HashMap<String, TextureCell>>,
    white: std::sync::Arc<GpuTexture>,
    videos: std::sync::Mutex<Vec<VideoTexture>>,
    atlases: std::sync::Mutex<HashMap<String, std::sync::Arc<AtlasTexture>>>,
}

fn blame_once(name: &str) {
    static SAID: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SAID.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut seen) = seen.lock()
        && seen.insert(name.to_owned())
    {
        tracing::warn!(texture = name, "texture will not load; drawing white");
    }
}

impl TextureRegistry {
    #[must_use]
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let white = std::sync::Arc::new(upload_rgba8(
            device,
            queue,
            "kirie-white",
            1,
            1,
            &[255, 255, 255, 255],
            true,
            true,
        ));
        TextureRegistry {
            device: device.clone(),
            queue: queue.clone(),
            cache: std::sync::Mutex::new(HashMap::new()),
            videos: std::sync::Mutex::new(Vec::new()),
            atlases: std::sync::Mutex::new(HashMap::new()),
            white,
        }
    }

    #[must_use]
    pub fn white(&self) -> std::sync::Arc<GpuTexture> {
        self.white.clone()
    }

    pub fn get_wrapping(&self, name: &str, source: &dyn AssetSource) -> std::sync::Arc<GpuTexture> {
        let key = format!("\u{0}wrap:{name}");
        let slot = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .clone();
        slot.get_or_init(|| {
            let plain = self.load(name, source)?;
            Some(std::sync::Arc::new(GpuTexture {
                sampler: self.device.create_sampler(&wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    address_mode_u: wgpu::AddressMode::Repeat,
                    address_mode_v: wgpu::AddressMode::Repeat,
                    ..Default::default()
                }),
                texture: plain.texture.clone(),
                view: plain.view.clone(),
                width: plain.width,
                height: plain.height,
                uv_crop: plain.uv_crop,
                real_size: plain.real_size,
            }))
        })
        .clone()
        .unwrap_or_else(|| self.get(name, source))
    }

    pub fn get(&self, name: &str, source: &dyn AssetSource) -> std::sync::Arc<GpuTexture> {
        let slot = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(name.to_string())
            .or_default()
            .clone();
        slot.get_or_init(|| self.load(name, source))
            .clone()
            .unwrap_or_else(|| {
                blame_once(name);
                self.white.clone()
            })
    }

    pub fn get_sprite_frame0(&self, name: &str, source: &dyn AssetSource) -> std::sync::Arc<GpuTexture> {
        let key = format!("\u{0}f0:{name}");
        let slot = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .clone();
        match slot.get_or_init(|| self.load_frame0(name, source)) {
            Some(t) => t.clone(),
            None => self.get(name, source),
        }
    }

    fn load_frame0(&self, name: &str, source: &dyn AssetSource) -> Option<std::sync::Arc<GpuTexture>> {
        let path = format!("materials/{name}.tex");
        let bytes = source.load(&path)?;
        let content = ImageContent::from_tex_bytes(&bytes).ok()?;
        if content.frames.len() <= 1 {
            return None;
        }
        let page = content.pages.first()?;
        let fr = content.frames.first()?;
        if fr.axes[1].abs() > 1e-4 || fr.axes[2].abs() > 1e-4 {
            return None;
        }
        let (tw, th) = (page.width as i64, page.height as i64);
        let fx = (fr.translation[0] * tw as f32).round() as i64;
        let fy = (fr.translation[1] * th as f32).round() as i64;
        let fw = ((fr.axes[0] * tw as f32).round() as i64).max(1);
        let fh = ((fr.axes[3] * th as f32).round() as i64).max(1);
        if fx < 0 || fy < 0 || fx + fw > tw || fy + fh > th {
            return None;
        }
        let (fx, fy, fw, fh) = (fx as usize, fy as usize, fw as usize, fh as usize);
        let stride = page.width as usize * 4;
        let mut cropped = Vec::with_capacity(fw * fh * 4);
        for row in 0..fh {
            let start = (fy + row) * stride + fx * 4;
            cropped.extend_from_slice(&page.pixels[start..start + fw * 4]);
        }
        let gpu = upload_rgba8(
            &self.device,
            &self.queue,
            name,
            fw as u32,
            fh as u32,
            &cropped,
            content.sampler.nearest,
            true,
        );
        Some(std::sync::Arc::new(gpu))
    }

    fn load(&self, name: &str, source: &dyn AssetSource) -> Option<std::sync::Arc<GpuTexture>> {
        let path = format!("materials/{name}.tex");
        let bytes = source.load(&path)?;
        let mut content = match ImageContent::from_tex_bytes(&bytes) {
            Ok(c) => c,
            Err(RenderError::VideoTex) => return self.load_video_first_frame(name, &bytes),
            Err(e) => {
                tracing::debug!(texture = %name, error = %e, "texture decode failed; using white");
                return None;
            }
        };
        content.pad_pages_to_max();
        let page = content.pages.first()?;
        let uv_crop = match content.frames.as_slice() {
            [only] => [only.axes[0], only.axes[3]],
            _ => [1.0, 1.0],
        };
        let mut gpu = upload_rgba8(
            &self.device,
            &self.queue,
            name,
            page.width,
            page.height,
            &page.pixels,
            content.sampler.nearest,
            content.sampler.clamp_uvs,
        );
        gpu.uv_crop = uv_crop;
        gpu.real_size = [content.content_width as f32, content.content_height as f32];
        let gpu = std::sync::Arc::new(gpu);
        self.register_atlas(name, content, &gpu);
        Some(gpu)
    }

    fn register_atlas(&self, name: &str, content: ImageContent, gpu: &std::sync::Arc<GpuTexture>) {
        let Some(multi_page) = atlas_animates(&content) else {
            if content.frames.len() > 1 {
                tracing::debug!(texture = %name, "animated .tex not streamable; keeping static frame 0");
            }
            return;
        };
        let schedule = content.schedule();
        self.atlases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                name.to_string(),
                std::sync::Arc::new(AtlasTexture {
                    frames: content.frames,
                    schedule,
                    pages: if multi_page { content.pages } else { Vec::new() },
                    gpu: gpu.clone(),
                }),
            );
    }

    #[must_use]
    pub fn atlas_for(&self, name: &str) -> Option<std::sync::Arc<AtlasTexture>> {
        self.atlases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    fn load_video_first_frame(&self, name: &str, tex_bytes: &[u8]) -> Option<std::sync::Arc<GpuTexture>> {
        use std::time::Duration;

        let tex = kirie_formats::tex::Tex::parse(tex_bytes).ok()?;
        let payload = tex.video_payload().ok()?;
        let mut key: u64 = 0xcbf2_9ce4_8422_2325;
        for b in &*payload {
            key = (key ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let file = std::env::temp_dir().join(format!("kirie-vtex-{key:016x}.mp4"));
        std::fs::write(&file, &payload).ok()?;

        let opened = kirie_video::VideoPlayer::open(
            &file,
            kirie_video::VideoOptions {
                enable_audio: false,
                silent: true,
                nv12: true,
                ..kirie_video::VideoOptions::default()
            },
        );
        let (player, frame) = match opened {
            Ok((player, control)) => {
                let frame = player.recv_frame_timeout(Duration::from_secs(5));
                (Some((player, control)), frame)
            }
            Err(e) => {
                tracing::debug!(texture = %name, error = %e, "video texture open failed; using white");
                (None, None)
            }
        };
        let _ = std::fs::remove_file(&file);
        let frame = frame?;
        if frame.width == 0 || frame.height == 0 {
            return None;
        }

        let is_nv12 = frame.pixels == kirie_video::FramePixels::Nv12;
        let rgba_seed: std::borrow::Cow<[u8]> = if is_nv12 {
            std::borrow::Cow::Owned(vec![0u8; (frame.width * frame.height * 4) as usize])
        } else {
            std::borrow::Cow::Borrowed(&frame.data)
        };
        let gpu = std::sync::Arc::new(upload_video_target(
            &self.device,
            &self.queue,
            name,
            frame.width,
            frame.height,
            &rgba_seed,
            tex.flags.no_interpolation(),
            tex.flags.clamp_uvs(),
        ));
        let nv12 = is_nv12.then(|| {
            let rig = Nv12Rig::new(
                &self.device,
                nv12_pipeline(&self.device),
                &gpu,
                frame.width,
                frame.height,
            );
            rig.convert(&self.device, &self.queue, frame.width, frame.height, &frame.data);
            rig
        });
        if let Some((player, control)) = player {
            self.videos
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(VideoTexture {
                    script_paused: std::cell::Cell::new(false),
                    name: name.to_owned(),
                    player,
                    control,
                    paused: std::cell::Cell::new(false),
                    gpu: gpu.clone(),
                    nv12,
                    size: (frame.width, frame.height),
                });
        }
        Some(gpu)
    }

    pub fn peek_video_names(&self) -> Vec<String> {
        self.videos
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|v| v.name.clone())
            .collect()
    }

    pub fn take_videos(&mut self) -> Vec<VideoTexture> {
        std::mem::take(
            &mut *self
                .videos
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub fn take_atlases(&mut self) -> Vec<std::sync::Arc<AtlasTexture>> {
        std::mem::take(
            &mut *self
                .atlases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_values()
        .collect()
    }
}

fn atlas_animates(content: &ImageContent) -> Option<bool> {
    if content.frames.len() <= 1 || !content.schedule().is_animated() {
        return None;
    }
    Some(content.frames.iter().any(|f| f.page != 0))
}

#[allow(clippy::too_many_arguments)]
fn upload_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    pixels: &[u8],
    nearest: bool,
    clamp: bool,
) -> GpuTexture {
    let width = width.max(1);
    let height = height.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let need = (width * height * 4) as usize;
    if pixels.len() >= need {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let filter = if nearest {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    let address = if clamp {
        wgpu::AddressMode::ClampToEdge
    } else {
        wgpu::AddressMode::Repeat
    };
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: address,
        address_mode_v: address,
        address_mode_w: address,
        mag_filter: filter,
        min_filter: filter,
        ..wgpu::SamplerDescriptor::default()
    });
    GpuTexture {
        texture,
        view,
        sampler,
        width,
        height,
        uv_crop: [1.0, 1.0],
        real_size: [width as f32, height as f32],
    }
}

#[cfg(test)]
mod tests {
    use crate::content::SamplerSpec;

    use super::*;

    fn content(pages: Vec<(u32, u32)>, frames: Vec<(usize, f32)>) -> ImageContent {
        ImageContent {
            pages: pages
                .into_iter()
                .map(|(width, height)| ImagePage {
                    width,
                    height,
                    pixels: vec![0; (width * height * 4) as usize],
                })
                .collect(),
            frames: frames
                .into_iter()
                .map(|(page, duration)| FramePlacement {
                    page,
                    duration,
                    translation: [0.0, 0.0],
                    axes: [1.0, 0.0, 0.0, 1.0],
                })
                .collect(),
            sampler: SamplerSpec {
                nearest: false,
                clamp_uvs: true,
            },
            content_width: 4,
            content_height: 4,
        }
    }

    #[test]
    fn spritesheets_animate_without_page_streaming() {
        let c = content(vec![(8, 8)], vec![(0, 0.1), (0, 0.1)]);
        assert_eq!(atlas_animates(&c), Some(false));
    }

    #[test]
    fn uniform_multi_page_gifs_stream_pages() {
        let c = content(vec![(4, 4), (4, 4)], vec![(0, 0.1), (1, 0.1)]);
        assert_eq!(atlas_animates(&c), Some(true));
    }

    #[test]
    fn static_and_malformed_content_never_animates() {
        let single = content(vec![(4, 4)], vec![(0, 0.0)]);
        assert_eq!(atlas_animates(&single), None);
        let zero = content(vec![(4, 4)], vec![(0, 0.0), (0, 0.0)]);
        assert_eq!(atlas_animates(&zero), None);
    }

    #[test]
    fn pages_of_different_sizes_still_animate() {
        let mismatched = content(vec![(4, 4), (8, 8)], vec![(0, 0.1), (1, 0.1)]);
        assert_eq!(atlas_animates(&mismatched), Some(true));
    }
}
