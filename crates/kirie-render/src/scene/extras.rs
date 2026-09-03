use std::sync::Arc;

use kirie_scene::material::Blending;
use kirie_scene::object::{Object, ParticleObject, TextObject};
use kirie_scene::particle::ParticleSystem;
use kirie_scene::resolve::AssetSource;
use wgpu::util::DeviceExt;

use crate::particle::{ParticleRenderer, ParticleSim, SimConfig};

use super::fbo::FBO_FORMAT;
use super::matrix::{self, Mat4};
use super::text::{self, TextFonts};
use super::texture::{GpuTexture, TextureRegistry};

pub struct ParticleGpu {
    pub id: i64,
    pub visible: bool,
    pub origin: [f32; 3],
    pub sim: ParticleSim,
    pub renderer: ParticleRenderer,
    pub view_projection: [f32; 16],
    _texture: Option<Arc<GpuTexture>>,
}

pub struct TextGpu {
    pub id: i64,
    pub visible: bool,
    pub blank: bool,
    pub bind: wgpu::BindGroup,
    pub vertex_buffer: wgpu::Buffer,
    _texture: GpuTexture,
    rebuild: TextRebuild,
    quad: TextQuad,
    ubo: wgpu::Buffer,
}

pub struct TextRebuild {
    pub current: String,
    font: String,
    raster_px: f32,
    box_w: f32,
    halign: String,
    valign: String,
    padding: f32,
    raster_scale: f32,
    bundled: Option<String>,
    box_scale: f32,
}

const WE_PT_TO_PX: f32 = 300.0 / 72.0;

impl TextRebuild {
    #[must_use]
    pub fn rasterize(&self, fonts: &mut TextFonts) -> text::TextRaster {
        text::rasterize(
            fonts,
            &self.current,
            &self.font,
            self.raster_px,
            self.box_w,
            &self.halign,
            self.padding,
            self.bundled.as_deref(),
        )
        .filter(|r| r.any_coverage)
        .unwrap_or_else(text::TextRaster::blank)
    }

    pub fn set_max_width(&mut self, maxwidth: f32) -> bool {
        let w = maxwidth * self.box_scale;
        if (w - self.box_w).abs() < f32::EPSILON {
            return false;
        }
        self.box_w = w;
        true
    }

    pub fn set_point_size(&mut self, pointsize: f32) -> bool {
        let px = pointsize * WE_PT_TO_PX * self.raster_scale;
        if (px - self.raster_px).abs() < f32::EPSILON {
            return false;
        }
        self.raster_px = px;
        true
    }

    pub fn set_font(&mut self, font: &str, fonts: &mut TextFonts, source: &dyn AssetSource) -> bool {
        if font == self.font {
            return false;
        }
        self.font = font.to_owned();
        self.bundled = fonts.bundled_family(font, source);
        true
    }

    pub fn quad_scale(&self, world_scale: [f32; 2]) -> [f32; 2] {
        [
            world_scale[0] / self.raster_scale,
            world_scale[1] / self.raster_scale,
        ]
    }
}

struct TextQuad {
    quad_scale: [f32; 2],
    raster_size: (u32, u32),
    anchor_box: [f32; 2],
    inset: f32,
    origin: [f32; 2],
    angle_z: f32,
    scene_size: (u32, u32),
    tint: [f32; 3],
    alpha: f32,
    color_alpha: f32,
}

pub type WorldXf = ([f32; 2], [f32; 2], f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRasterScale {
    Screen,
    Engine,
}

#[must_use]
pub fn text_layout(
    fonts: &mut TextFonts,
    tobj: &TextObject,
    source: &dyn AssetSource,
    world: ([f32; 2], [f32; 2]),
    raster: TextRasterScale,
) -> Option<(TextRebuild, text::TextRaster)> {
    let bundled = fonts.bundled_family(&tobj.font, source);
    let scale_x = tobj.scale.value[0];
    let scale_y = tobj.scale.value[1];
    if scale_x <= 0.0 || scale_y <= 0.0 {
        return None;
    }
    let (_, world_scale) = world;
    let raster_scale = match raster {
        TextRasterScale::Screen => ((world_scale[0] + world_scale[1]) * 0.5).clamp(0.05, 32.0),
        TextRasterScale::Engine => 1.0,
    };
    let raster_px = tobj.pointsize.value * WE_PT_TO_PX * raster_scale;
    let box_scale = if tobj.limitwidth { raster_scale } else { 0.0 };
    let rebuild = TextRebuild {
        current: tobj.text.value.clone(),
        font: tobj.font.clone(),
        raster_px,
        box_w: tobj.maxwidth.value * box_scale,
        halign: tobj.horizontalalign.clone(),
        valign: tobj.verticalalign.clone(),
        padding: tobj.padding as f32 * raster_scale,
        raster_scale,
        bundled,
        box_scale,
    };
    let raster = rebuild.rasterize(fonts);
    Some((rebuild, raster))
}

#[must_use]
pub fn text_anchor(raster: &text::TextRaster, halign: &str, valign: &str) -> [f32; 2] {
    let (w, h) = (raster.width.max(1) as f32, raster.height.max(1) as f32);
    let reach = 1.0 - 2.0 * raster.inset / w;
    let x = match halign {
        "left" => reach,
        "right" => -reach,
        _ => 0.0,
    };
    let along = match valign {
        "top" => 0.0,
        "bottom" => 1.0,
        _ => 0.5,
    };
    let y = 2.0 * (raster.anchor_box[0] + along * raster.anchor_box[1]) / h - 1.0;
    [x, y]
}

impl TextGpu {
    #[must_use]
    pub fn current_text(&self) -> &str {
        &self.rebuild.current
    }

    pub fn set_transform(&mut self, device: &wgpu::Device, world: WorldXf) {
        self.quad.origin = world.0;
        self.quad.quad_scale = self.rebuild.quad_scale(world.1);
        self.quad.angle_z = world.2;
        self.rebuild_quad(device);
    }

    fn rebuild_quad(&mut self, device: &wgpu::Device) {
        let q = &self.quad;
        let sx = q.raster_size.0 as f32 * q.quad_scale[0];
        let sy = q.raster_size.1 as f32 * q.quad_scale[1];
        let anchor = [
            q.anchor_box[0] * q.quad_scale[1],
            q.anchor_box[1] * q.quad_scale[1],
        ];
        let [cx, cy] = anchored_center(
            q.origin,
            [sx, sy],
            anchor,
            q.inset * q.quad_scale[0],
            &self.rebuild.halign,
            &self.rebuild.valign,
        );
        let quad = rotated_about_origin(
            scene_space_quad(cx, cy, sx, sy, q.scene_size),
            q.origin,
            q.angle_z,
            q.scene_size,
        );
        let uvs: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]];
        let mut verts = Vec::with_capacity(4 * 20);
        for (p, uv) in quad.iter().zip(uvs.iter()) {
            for &f in p {
                verts.extend_from_slice(&f.to_le_bytes());
            }
            for &f in uv {
                verts.extend_from_slice(&f.to_le_bytes());
            }
        }
        self.vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("kirie-scene-text-vb"),
            contents: &verts,
            usage: wgpu::BufferUsages::VERTEX,
        });
    }

    pub fn set_tint(&mut self, queue: &wgpu::Queue, color: Option<[f32; 3]>, alpha: Option<f32>) {
        let q = &mut self.quad;
        if let Some(c) = color {
            q.tint = c;
        }
        if let Some(a) = alpha {
            q.alpha = a;
        }
        let data = [q.tint[0], q.tint[1], q.tint[2], q.alpha * q.color_alpha];
        queue.write_buffer(&self.ubo, 64, bytemuck::cast_slice(&data));
    }

    pub fn set_max_width(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tp: &TextPipeline,
        fonts: &mut TextFonts,
        maxwidth: f32,
    ) {
        if self.rebuild.set_max_width(maxwidth) {
            self.rerasterize(device, queue, tp, fonts);
        }
    }

    pub fn set_point_size(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tp: &TextPipeline,
        fonts: &mut TextFonts,
        pointsize: f32,
    ) {
        if self.rebuild.set_point_size(pointsize) {
            self.rerasterize(device, queue, tp, fonts);
        }
    }

    pub fn set_font(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tp: &TextPipeline,
        fonts: &mut TextFonts,
        source: &dyn AssetSource,
        font: &str,
    ) {
        if self.rebuild.set_font(font, fonts, source) {
            self.rerasterize(device, queue, tp, fonts);
        }
    }

    pub fn retext(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tp: &TextPipeline,
        fonts: &mut TextFonts,
        new_text: &str,
    ) {
        if new_text == self.rebuild.current {
            return;
        }
        self.rebuild.current = new_text.to_owned();
        self.rerasterize(device, queue, tp, fonts);
    }

    fn rerasterize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tp: &TextPipeline,
        fonts: &mut TextFonts,
    ) {
        let raster = self.rebuild.rasterize(fonts);
        if !raster.any_coverage {
            self.blank = true;
            return;
        }
        self.blank = false;
        let texture = text::upload(device, queue, &raster);
        self.quad.raster_size = (raster.width, raster.height);
        self.quad.anchor_box = raster.anchor_box;
        self.quad.inset = raster.inset;
        self.rebuild_quad(device);
        self.bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-scene-text-bg"),
            layout: &tp.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
            ],
        });
        self._texture = texture;
    }
}

pub struct TextPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_particle(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    object: &Object,
    pobj: &ParticleObject,
    scene_size: (u32, u32),
    screen_mvp: &Mat4,
    source: &dyn AssetSource,
    registry: &mut TextureRegistry,
) -> Option<ParticleGpu> {
    if !(pobj.visible.value && object.base.visible.value) {
        return None;
    }

    let sim = ParticleSim::new(
        &pobj.system,
        &pobj.instanceoverride,
        SimConfig {
            seed: 0x00C0_FFEE ^ (object.base.id as u64),
            sheet: None,
        },
    );
    let capacity = sim.capacity();

    let (texture, blending) = particle_material(&pobj.system, source, registry);
    let tex_ref = texture.as_ref().map(|t| (&t.view, &t.sampler));
    let renderer = ParticleRenderer::new(device, queue, FBO_FORMAT, blending, tex_ref, capacity);

    let model = particle_model_matrix(object, pobj, scene_size);
    let view_projection = matrix::mul(screen_mvp, &model);

    Some(ParticleGpu {
        id: object.base.id,
        visible: true,
        origin: object.base.origin.value,
        sim,
        renderer,
        view_projection,
        _texture: texture,
    })
}

fn particle_material(
    system: &ParticleSystem,
    source: &dyn AssetSource,
    registry: &mut TextureRegistry,
) -> (Option<Arc<GpuTexture>>, Blending) {
    let Some(pass) = system.resolved_material.as_ref().and_then(|m| m.passes.first()) else {
        return (None, Blending::Additive);
    };
    let texture = pass
        .textures
        .first()
        .and_then(|slot| slot.clone())
        .filter(|n| !n.starts_with("_rt_") && !n.starts_with("_alias_"))
        .map(|n| registry.get_sprite_frame0(&n, source));
    (texture, pass.blending)
}

fn particle_model_matrix(object: &Object, pobj: &ParticleObject, scene_size: (u32, u32)) -> Mat4 {
    let (sw, sh) = (scene_size.0 as f32, scene_size.1 as f32);
    let o = object.base.origin.value;
    let t = matrix::translation([o[0] - sw / 2.0, o[1] - sh / 2.0, o[2]]);
    let a = pobj.angles.value;
    let rz = matrix::rotation_z(-a[2]);
    let ry = matrix::rotation_y(a[1]);
    let rx = matrix::rotation_x(-a[0]);
    let s = matrix::scale(pobj.scale.value);
    matrix::mul(&t, &matrix::mul(&rz, &matrix::mul(&ry, &matrix::mul(&rx, &s))))
}

#[must_use]
pub fn build_text_pipeline(device: &wgpu::Device) -> TextPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-scene-text-shader"),
        source: wgpu::ShaderSource::Wgsl(TEXT_WGSL.into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kirie-scene-text-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kirie-scene-text-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: 20,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2],
    };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-scene-text-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout)],
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
                format: FBO_FORMAT,
                blend: Some(super::blend::blend_state(Blending::Translucent)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    TextPipeline { pipeline, bgl }
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_text(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tp: &TextPipeline,
    fonts: &mut TextFonts,
    object: &Object,
    tobj: &TextObject,
    scene_size: (u32, u32),
    screen_mvp: &Mat4,
    source: &dyn AssetSource,
    world: WorldXf,
) -> Option<TextGpu> {
    let visible = tobj.visible.value && object.base.visible.value;
    let (world_origin, world_scale, _) = world;
    let (rebuild, raster) = text_layout(
        fonts,
        tobj,
        source,
        (world_origin, world_scale),
        TextRasterScale::Screen,
    )?;
    let blank = !raster.any_coverage;
    let texture = text::upload(device, queue, &raster);

    let quad_scale = rebuild.quad_scale(world_scale);
    let sx = raster.width as f32 * quad_scale[0];
    let sy = raster.height as f32 * quad_scale[1];
    let anchor = [
        raster.anchor_box[0] * quad_scale[1],
        raster.anchor_box[1] * quad_scale[1],
    ];
    let [cx, cy] = anchored_center(
        world_origin,
        [sx, sy],
        anchor,
        raster.inset * quad_scale[0],
        &tobj.horizontalalign,
        &tobj.verticalalign,
    );
    let quad = rotated_about_origin(
        scene_space_quad(cx, cy, sx, sy, scene_size),
        world_origin,
        world.2,
        scene_size,
    );
    let uvs: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]];

    let color = tobj.color.value;
    let alpha = tobj.alpha.value * color[3];
    let mut data = Vec::with_capacity(80);
    for f in screen_mvp {
        data.extend_from_slice(&f.to_le_bytes());
    }
    for f in [color[0], color[1], color[2], alpha] {
        data.extend_from_slice(&f.to_le_bytes());
    }
    let ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("kirie-scene-text-ubo"),
        contents: &data,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kirie-scene-text-bg"),
        layout: &tp.bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&texture.view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&texture.sampler),
            },
        ],
    });

    let mut verts = Vec::with_capacity(4 * 20);
    for (p, uv) in quad.iter().zip(uvs.iter()) {
        for &f in p {
            verts.extend_from_slice(&f.to_le_bytes());
        }
        for &f in uv {
            verts.extend_from_slice(&f.to_le_bytes());
        }
    }
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("kirie-scene-text-vb"),
        contents: &verts,
        usage: wgpu::BufferUsages::VERTEX,
    });
    Some(TextGpu {
        id: object.base.id,
        visible,
        blank,
        bind,
        vertex_buffer,
        _texture: texture,
        rebuild,
        quad: TextQuad {
            quad_scale,
            raster_size: (raster.width, raster.height),
            anchor_box: raster.anchor_box,
            inset: raster.inset,
            origin: [world_origin[0], world_origin[1]],
            angle_z: world.2,
            scene_size,
            tint: [color[0], color[1], color[2]],
            alpha: tobj.alpha.value,
            color_alpha: color[3],
        },
        ubo,
    })
}

pub fn draw_text(
    encoder: &mut wgpu::CommandEncoder,
    tp: &TextPipeline,
    text: &TextGpu,
    scene_view: &wgpu::TextureView,
) {
    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("kirie-scene-text-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: scene_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    rp.set_pipeline(&tp.pipeline);
    rp.set_bind_group(0, &text.bind, &[]);
    rp.set_vertex_buffer(0, text.vertex_buffer.slice(..));
    rp.draw(0..4, 0..1);
}

fn anchored_center(
    origin: [f32; 2],
    size: [f32; 2],
    anchor_box: [f32; 2],
    inset: f32,
    halign: &str,
    valign: &str,
) -> [f32; 2] {
    let x = match halign {
        "left" => origin[0] + size[0] / 2.0 - inset,
        "right" => origin[0] - size[0] / 2.0 + inset,
        _ => origin[0],
    };
    let along = match valign {
        "top" => 0.0,
        "bottom" => 1.0,
        _ => 0.5,
    };
    let y = origin[1] + anchor_box[0] + along * anchor_box[1] - size[1] / 2.0;
    [x, y]
}

fn rotated_about_origin(
    mut quad: [[f32; 3]; 4],
    origin: [f32; 2],
    angle_z: f32,
    scene: (u32, u32),
) -> [[f32; 3]; 4] {
    if angle_z == 0.0 {
        return quad;
    }
    let px = origin[0] - scene.0 as f32 / 2.0;
    let py = origin[1] - scene.1 as f32 / 2.0;
    let (s, c) = (-angle_z).sin_cos();
    for p in &mut quad {
        let (dx, dy) = (p[0] - px, p[1] - py);
        p[0] = px + dx * c - dy * s;
        p[1] = py + dx * s + dy * c;
    }
    quad
}

fn scene_space_quad(ox: f32, oy: f32, sx: f32, sy: f32, scene: (u32, u32)) -> [[f32; 3]; 4] {
    let (sw, sh) = (scene.0 as f32, scene.1 as f32);
    let left = (ox - sw / 2.0 - sx / 2.0 + 0.5).floor();
    let top = (oy - sh / 2.0 + sy / 2.0).floor();
    [
        [left, top, 0.0],
        [left, top - sy, 0.0],
        [left + sx, top, 0.0],
        [left + sx, top - sy, 0.0],
    ]
}

const TEXT_WGSL: &str = r#"
struct U { mvp: mat4x4<f32>, color: vec4<f32> }
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var cov_tex: texture_2d<f32>;
@group(0) @binding(2) var cov_smp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@location(0) p: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.pos = u.mvp * vec4<f32>(p, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coverage = textureSample(cov_tex, cov_smp, in.uv).a;
    return vec4<f32>(u.color.rgb, u.color.a * coverage);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_quad_uses_the_image_y_up_convention() {
        let q = scene_space_quad(100.0, 100.0, 200.0, 50.0, (1920, 1080));
        let cx = (q[0][0] + q[3][0]) / 2.0;
        let cy = (q[0][1] + q[3][1]) / 2.0;
        assert_eq!([cx, cy], [100.0 - 960.0, 100.0 - 540.0]);
        assert_eq!(q[0], [-960.0, -415.0, 0.0]);
        assert_eq!(q[1], [-960.0, -465.0, 0.0]);
        assert_eq!(q[2], [-760.0, -415.0, 0.0]);
        assert_eq!(q[3], [-760.0, -465.0, 0.0]);
    }

    #[test]
    fn text_quad_rotates_about_the_layer_origin() {
        let scene = (1920, 1080);
        let flat = scene_space_quad(1100.0, 540.0, 200.0, 50.0, scene);
        assert_eq!(rotated_about_origin(flat, [1000.0, 540.0], 0.0, scene), flat);
        let q = rotated_about_origin(flat, [1000.0, 540.0], 90f32.to_radians(), scene);
        let cx = (q[0][0] + q[3][0]) / 2.0;
        let cy = (q[0][1] + q[3][1]) / 2.0;
        assert!((cx - 40.0).abs() < 1e-3, "centre x {cx}");
        assert!((cy + 100.0).abs() < 1e-3, "centre y {cy}");
        let w = ((q[2][0] - q[0][0]).powi(2) + (q[2][1] - q[0][1]).powi(2)).sqrt();
        let h = ((q[1][0] - q[0][0]).powi(2) + (q[1][1] - q[0][1]).powi(2)).sqrt();
        assert!((w - 200.0).abs() < 1e-3 && (h - 50.0).abs() < 1e-3);
        assert!((q[2][0] - q[0][0]).abs() < 1e-3, "the top edge now runs vertically");
    }

    #[test]
    fn particles_share_the_image_y_up_convention() {
        let scene = (1920u32, 1080u32);
        let oy = 900.0_f32;
        let quad = scene_space_quad(0.0, oy, 10.0, 10.0, scene);
        let quad_cy = (quad[0][1] + quad[1][1]) / 2.0;
        let particle_cy = oy - scene.1 as f32 / 2.0;
        assert_eq!(quad_cy, particle_cy);
        assert!(particle_cy > 0.0, "a high origin sits above the centre");
    }

    #[test]
    fn text_and_image_conventions_agree() {
        let oy = 470.0;
        let q = scene_space_quad(0.0, oy, 10.0, 10.0, (1920, 1080));
        let text_cy = (q[0][1] + q[1][1]) / 2.0;
        let image_cy = oy - 540.0;
        assert_eq!(text_cy, image_cy);
    }

    #[test]
    fn left_aligned_text_starts_at_its_origin() {
        let c = anchored_center([-158.0, 62.0], [80.0, 30.0], [0.0, 30.0], 0.0, "left", "center");
        assert_eq!(c, [-118.0, 62.0]);
    }

    #[test]
    fn right_aligned_text_ends_at_its_origin() {
        let c = anchored_center(
            [375.0, -177.0],
            [200.0, 30.0],
            [0.0, 30.0],
            0.0,
            "right",
            "center",
        );
        assert_eq!(c, [275.0, -177.0]);
    }

    #[test]
    fn top_and_bottom_aligned_text_hang_from_their_origin() {
        assert_eq!(
            anchored_center([0.0, 10.0], [4.0, 20.0], [0.0, 20.0], 0.0, "center", "top"),
            [0.0, 0.0]
        );
        assert_eq!(
            anchored_center([0.0, 10.0], [4.0, 20.0], [0.0, 20.0], 0.0, "center", "bottom"),
            [0.0, 20.0]
        );
    }

    #[test]
    fn text_aligns_by_its_ascender_box_not_its_bitmap() {
        let size = [100.0, 84.0];
        let ascender_box = [0.0, 56.0];
        assert_eq!(
            anchored_center([0.0, 0.0], size, ascender_box, 0.0, "center", "top"),
            [0.0, -42.0]
        );
        assert_eq!(
            anchored_center([0.0, 0.0], size, ascender_box, 0.0, "center", "center"),
            [0.0, -14.0]
        );
        assert_eq!(
            anchored_center([0.0, 0.0], size, ascender_box, 0.0, "center", "bottom"),
            [0.0, 14.0]
        );
    }

    #[test]
    fn text_quads_snap_to_whole_scene_pixels() {
        let q = scene_space_quad(1763.0, 967.0, 37.0, 39.0, (1920, 1080));
        assert_eq!(q[0], [785.0, 446.0, 0.0]);
        assert_eq!(q[3], [822.0, 407.0, 0.0]);
    }

    #[test]
    fn glyph_top_lands_on_the_up_edge() {
        let q = scene_space_quad(0.0, 0.0, 100.0, 40.0, (1920, 1080));
        assert!(q[0][1] > q[1][1], "TL (v=0) above BL (v=1)");
        assert!(q[2][1] > q[3][1], "TR (v=0) above BR (v=1)");
    }
}
