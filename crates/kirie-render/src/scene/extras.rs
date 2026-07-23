//! Non-image scene-object compositing: particles and text placeholders
//! (docs/render-architecture.md §7.3 particles, §7.4 text).
//!
//! The image layer path lives in [`super::renderer`]; this module wires the
//! remaining renderable object kinds into the same scene-FBO composite so the
//! final frame is not image-only (SPEC.md §T16b scene fidelity):
//!
//! - **Particles** — one CPU [`ParticleSim`] + one instanced [`ParticleRenderer`]
//!   per particle object (the simulation/renderer already exist in
//!   `crate::particle`; this only *wires them in*, docs §7.3). The system's
//!   material supplies the sprite texture + blend mode; the object's transform
//!   (origin → centered Y-up, `rotZYX(-z, y, -x)`, scale) is folded once into
//!   the scene view-projection (docs §7.3 projection). Simulation advances each
//!   frame by `dt`; sprites are written into a caller-owned scratch buffer so
//!   there is no steady-state allocation (SPEC.md §V5).
//! - **Text** — real glyph rendering (docs §7.4). The object's string is shaped
//!   and rasterized once with cosmic-text into a coverage bitmap (font
//!   family/size/alignment from the [`TextObject`]; see [`super::text`]),
//!   uploaded as one texture, and drawn as a single textured quad at the text
//!   object's scene-space transform + z-order. The fragment shader multiplies
//!   coverage by the object's `color`/`alpha` and translucent-blends into the
//!   scene FBO. Shaping happens at build time, not per frame (SPEC.md §V5).
//!
//! Light / shape / model / sound / group objects are intentionally *not* drawn:
//! the reference dispatches everything except image/text/particle/model to an
//! invisible transform group, and the corpus model is an unparsed binary mesh
//! (docs §5 step 6, §7.2) — rendering a stand-in would move the composite
//! *away* from the C++ oracle, so [`super::renderer`] skips them with a trace.

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

/// One wired-in particle object: its CPU simulation, the GPU sprite renderer,
/// and the static scene view-projection (screen MVP × object model matrix,
/// folded once — the transform does not animate here, docs §7.3).
pub struct ParticleGpu {
    /// The owning scene-object id (script `sortLayer` targeting).
    pub id: i64,
    /// The CPU particle simulation (advanced per frame by `dt`).
    pub sim: ParticleSim,
    /// The instanced-quad sprite renderer targeting the scene FBO.
    pub renderer: ParticleRenderer,
    /// `screen_mvp × model` (column-major, 16 floats) uploaded each frame.
    pub view_projection: [f32; 16],
    /// The sprite texture, kept alive alongside the renderer's bind group.
    _texture: Option<Arc<GpuTexture>>,
}

/// One rendered text object: its glyph-coverage texture, the per-object bind
/// group (MVP + color uniform, coverage texture + sampler) and the textured quad
/// (docs §7.4). Draws with [`TextPipeline`].
pub struct TextGpu {
    /// The owning scene-object id (script `sortLayer` targeting).
    pub id: i64,
    /// The per-object bind group (uniform + coverage texture + sampler).
    pub bind: wgpu::BindGroup,
    /// The scene-space quad vertices (4-vertex triangle strip, pos + uv).
    pub vertex_buffer: wgpu::Buffer,
    /// The uploaded coverage texture, kept alive alongside the bind group.
    _texture: GpuTexture,
    /// The text-layer script binding (a WE clock/date script driving the
    /// string per frame, `CText::initScriptLayer`), if the object has one.
    /// `handle` is wired after the scene's script host starts.
    pub script: Option<TextScriptState>,
    /// Everything needed to re-rasterize the quad when the script changes the
    /// string (see [`TextGpu::retext`]).
    rebuild: TextRebuild,
    /// The uniform buffer (mvp + color), reused across retexts.
    ubo: wgpu::Buffer,
}

/// A text object's script driver (docs §7.2): source + scriptproperties from
/// the scene JSON, and the live layer handle once created.
pub struct TextScriptState {
    /// JS source of the layer script.
    pub source: String,
    /// The script's `scriptproperties` blob.
    pub properties: serde_json::Value,
    /// Script-engine layer handle (`None` until wired / on failure).
    pub handle: Option<u32>,
}

/// Re-rasterization context retained per text object. Layout parameters are
/// stored pre-resolved (raster pixel size, wrap box, effective padding, the
/// bitmap-pixel→scene-unit quad factors) so [`TextGpu::retext`] reproduces
/// [`build_text`]'s layout exactly.
struct TextRebuild {
    current: String,
    font: String,
    /// FreeType/cosmic pixel size the string is shaped at.
    raster_px: f32,
    /// Wrap-box width in raster pixels; 0 = never wrap (`limitwidth` false).
    box_w: f32,
    halign: String,
    valign: String,
    /// Effective padding in raster pixels (0 without a wrap box).
    padding: f32,
    /// Bitmap-pixel → scene-unit factor per axis (`scale / raster_scale`).
    quad_scale: [f32; 2],
    origin: [f32; 2],
    scene_size: (u32, u32),
    bundled: Option<String>,
}

impl TextGpu {
    /// The string currently rasterized.
    #[must_use]
    pub fn current_text(&self) -> &str {
        &self.rebuild.current
    }

    /// Re-rasterize with `new_text` (a script changed the string): new
    /// coverage texture + quad + bind group, reusing the pipeline, fonts and
    /// uniform buffer. No-ops when the string is unchanged; keeps the old
    /// visuals when the new string rasterizes to nothing (whitespace).
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
        let rb = &self.rebuild;
        let Some(raster) = text::rasterize(
            fonts,
            &rb.current,
            &rb.font,
            rb.raster_px,
            [rb.box_w, 0.0],
            &rb.halign,
            &rb.valign,
            rb.padding,
            rb.bundled.as_deref(),
        ) else {
            return;
        };
        if !raster.any_coverage {
            return;
        }
        let texture = text::upload(device, queue, &raster);
        // Quad = rasterized block mapped back into scene units (see
        // build_text: bitmap px × scale / raster_scale).
        let sx = raster.width as f32 * rb.quad_scale[0];
        let sy = raster.height as f32 * rb.quad_scale[1];
        let quad = scene_space_quad(rb.origin[0], rb.origin[1], sx, sy, rb.scene_size);
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

/// The shared glyph pipeline every [`TextGpu`] draws with (built once when the
/// scene has at least one drawable text object).
pub struct TextPipeline {
    /// The render pipeline (pos+uv vertex, uniform MVP + color, coverage sample).
    pub pipeline: wgpu::RenderPipeline,
    /// Its bind-group layout (uniform buffer + texture + sampler).
    pub bgl: wgpu::BindGroupLayout,
}

/// Build one particle object's simulation + renderer, or `None` when the object
/// (or its scene-object visibility) is hidden (docs §7.3; SPEC.md §V9 never
/// panics — an empty/unsupported system simply never spawns).
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
            // Deterministic per object so repeated builds match (tests, bake).
            seed: 0x00C0_FFEE ^ (object.base.id as u64),
            // Spritesheet frame timing needs the `.tex` grid, which the uploaded
            // GpuTexture does not expose — particles stay on frame 0 (documented
            // gap; single-sprite atlases are unaffected, docs §7.3).
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
        sim,
        renderer,
        view_projection,
        _texture: texture,
    })
}

/// The particle system's sprite texture (material pass 0, slot 0) and blend
/// mode (docs §7.3: the material's WE particle shader; here only the texture +
/// blend are needed since [`ParticleRenderer`] owns the sprite shader). Falls
/// back to no texture (→ the renderer's built-in white) and additive blending —
/// the corpus-dominant particle blend — when the material is absent.
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
        // Particle sprite sheets bind as a single frame; the instanced renderer
        // samples 0..1, so an atlas would otherwise draw its whole grid per
        // particle (docs §7.3 frame-0 seam).
        .map(|n| registry.get_sprite_frame0(&n, source));
    (texture, pass.blending)
}

/// The particle system's static model matrix (docs §7.3 projection):
/// `translate(centered origin) × rotZYX(-z, y, -x) × scale`. The origin is
/// converted from JSON Y-down screen space to centered Y-up
/// (`x -= sceneW/2; y = sceneH/2 - y`). Parallax and angle-animation are not
/// applied (documented gap — static transform).
fn particle_model_matrix(object: &Object, pobj: &ParticleObject, scene_size: (u32, u32)) -> Mat4 {
    let (sw, sh) = (scene_size.0 as f32, scene_size.1 as f32);
    let o = object.base.origin.value;
    let t = matrix::translation([o[0] - sw / 2.0, sh / 2.0 - o[1], o[2]]);
    let a = pobj.angles.value;
    let rz = matrix::rotation_z(-a[2]);
    let ry = matrix::rotation_y(a[1]);
    let rx = matrix::rotation_x(-a[0]);
    let s = matrix::scale(pobj.scale.value);
    matrix::mul(&t, &matrix::mul(&rz, &matrix::mul(&ry, &matrix::mul(&rx, &s))))
}

/// Build the shared text pipeline: a textured quad transformed by a per-object
/// MVP, its glyph coverage multiplied by a per-object color and translucent-
/// blended into the scene FBO (docs §7.4 `CText` state:
/// `GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA`).
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
        // pos vec3 + uv vec2
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

/// Shape + rasterize one text object into a coverage texture and build its
/// textured quad, or `None` when the object is hidden, its string is empty, or
/// the layout produced no glyph coverage (docs §7.4; SPEC.md §V9 — malformed
/// text is skipped, never panics). Shaping happens here (build time), not per
/// frame (SPEC.md §V5).
///
/// The quad is sized from the rasterized block (glyphs shaped at
/// `pointsize × 300/72 × scale` — WE points are 300 DPI, back-solved from
/// editor previews) and centered on the object origin in the same Y-up
/// convention as image layers (see [`scene_space_quad`]). Word wrap happens
/// only when `limitwidth` is set, at `maxwidth` scene units.
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
) -> Option<TextGpu> {
    if !(tobj.visible.value && object.base.visible.value) {
        return None;
    }

    // Load the wallpaper's own packaged font (docs §13) so shaping uses it
    // instead of a system substitute; `None` keeps the fallback path.
    let bundled = fonts.bundled_family(&tobj.font, source);
    // Windows text sizing, back-solved from editor previews of real
    // wallpapers (3587565260's clock: pointsize 22, scale 1.3485 measures
    // 123±2 px em on a 1919-unit-high scene ⇒ px/pt ≈ 4.17 = 300/72; both
    // its width and height solve to the same factor): the on-screen glyph em
    // is `pointsize × 300/72 × scale` scene units — WE points are 300 DPI.
    // Rasterize with the object scale folded in (crisp glyphs instead of a
    // stretched quad) and map bitmap pixels back by `scale / raster_scale`.
    const WE_PT_TO_PX: f32 = 300.0 / 72.0;
    let scale_x = tobj.scale.value[0];
    let scale_y = tobj.scale.value[1];
    if scale_x <= 0.0 || scale_y <= 0.0 {
        return None;
    }
    let raster_scale = ((scale_x + scale_y) * 0.5).clamp(0.05, 32.0);
    let raster_px = tobj.pointsize.value * WE_PT_TO_PX * raster_scale;
    // Wrap only when the author asked for it: `limitwidth` gates word wrap
    // at `maxwidth` scene units. The editor's `size` box is a gizmo, not a
    // layout constraint — wrapping at it broke single-line text (3421423611's
    // "WEDNESDAY" wrapped after 8 glyphs). Padding insets only within a box.
    let box_w = if tobj.limitwidth {
        tobj.maxwidth * raster_scale / scale_x
    } else {
        0.0
    };
    let padding = if tobj.limitwidth {
        tobj.padding as f32 * raster_scale
    } else {
        0.0
    };
    let raster = text::rasterize(
        fonts,
        &tobj.text.value,
        &tobj.font,
        raster_px,
        [box_w, 0.0],
        &tobj.horizontalalign,
        &tobj.verticalalign,
        padding,
        bundled.as_deref(),
    )?;
    // Nothing visible would draw (whitespace-only text, or no font faces
    // available): skip rather than upload a fully-transparent quad (V6/V9).
    if !raster.any_coverage {
        return None;
    }
    let texture = text::upload(device, queue, &raster);

    // Scene-space quad: bitmap pixels mapped back into scene units
    // (× scale / raster_scale — near-1 for uniform scales), centered on the
    // origin in the image-layer Y-up convention.
    let quad_scale = [scale_x / raster_scale, scale_y / raster_scale];
    let sx = raster.width as f32 * quad_scale[0];
    let sy = raster.height as f32 * quad_scale[1];
    let origin = object.base.origin.value;
    let quad = scene_space_quad(origin[0], origin[1], sx, sy, scene_size);
    // UVs: TL, BL, TR, BR — v = 0 at the top row of the bitmap.
    let uvs: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]];

    // Uniform: mvp (16 floats) then color (4 floats), std140-compatible.
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
        usage: wgpu::BufferUsages::UNIFORM,
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
        bind,
        vertex_buffer,
        _texture: texture,
        script: tobj.text.script.as_ref().map(|sb| TextScriptState {
            source: sb.source.clone(),
            properties: serde_json::Value::Object(sb.properties.clone()),
            handle: None,
        }),
        rebuild: TextRebuild {
            current: tobj.text.value.clone(),
            font: tobj.font.clone(),
            raster_px,
            box_w,
            halign: tobj.horizontalalign.clone(),
            valign: tobj.verticalalign.clone(),
            padding,
            quad_scale,
            origin: [origin[0], origin[1]],
            scene_size,
            bundled,
        },
        ubo,
    })
}

/// Draw a text object's glyph quad into the scene FBO (loads existing contents
/// so it composites over the layers below it, docs §7.4).
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

/// The **text** scene-space quad (TL, BL, TR, BR triangle strip) for a glyph
/// block of pixel size `sx × sy` centered on the text `origin`.
///
/// Text origins use the SAME Y-up convention as image layers (the origin is
/// the block center; on screen the text sits `origin.y` rows from the scene
/// BOTTOM). Windows-truth, verified against editor previews of real
/// wallpapers (3587565260's clock: origin (3215, 1870) in a 3413x1919 scene
/// renders 49 units from the top ⇔ Y-up; 3421423611's three text objects
/// mirror-match the same way). The Linux C++ port's `CText` placed text
/// `origin.y` rows from the TOP instead — that port's text path is an
/// admitted phase-1 reimplementation ("multi-line wrapping, alignment, and
/// padding come with Phase 2", CText.cpp:233-234), not reversed reference
/// behavior, and its convention mirrored every text object about the
/// horizontal center.
///
/// Orientation: the caller's UV order (TL `v=0` … BR `v=1`) puts the glyph
/// top row on the `+hh` (screen-top) edge, keeping glyphs upright.
fn scene_space_quad(ox: f32, oy: f32, sx: f32, sy: f32, scene: (u32, u32)) -> [[f32; 3]; 4] {
    let (sw, sh) = (scene.0 as f32, scene.1 as f32);
    let (hw, hh) = (sx / 2.0, sy / 2.0);
    // The text origin is the quad CENTER in the same Y-up scene space as
    // image layers (`renderer.rs::scene_space_quad`: `cy = oy - sh/2`).
    // Windows-truth, measured against editor previews (Komi 3587565260:
    // origin (3215, 1870) in a 3413x1919 scene lands the clock 49 units from
    // the TOP, i.e. Y-up) — the old `sh/2 - oy` "CText vflip" convention came
    // from the incomplete Linux C++ port (its CText is a phase-1
    // reimplementation, not reversed behavior) and mirrored every text
    // object about the horizontal center.
    let cx = ox - sw / 2.0;
    let cy = oy - sh / 2.0;
    [
        [cx - hw, cy + hh, 0.0],
        [cx - hw, cy - hh, 0.0],
        [cx + hw, cy + hh, 0.0],
        [cx + hw, cy - hh, 0.0],
    ]
}

/// The text shader: a scene-space quad transformed by the object MVP, sampling
/// the glyph-coverage texture and multiplying it by the object color/alpha
/// (docs §7.4). The coverage lives in the texture's alpha channel (RGB is 0xFF).
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
        // Windows-truth (editor previews, e.g. 3587565260's clock): the text
        // origin is the block center in the SAME Y-up space as image layers.
        // JSON origin (100, 100) in a 1920x1080 scene sits 100 rows from the
        // scene BOTTOM, i.e. kirie center-y = 100 - 540 = -440.
        let q = scene_space_quad(100.0, 100.0, 200.0, 50.0, (1920, 1080));
        let cx = (q[0][0] + q[3][0]) / 2.0;
        let cy = (q[0][1] + q[3][1]) / 2.0;
        assert_eq!([cx, cy], [100.0 - 960.0, 100.0 - 540.0]);
        // Corner layout TL, BL, TR, BR at the scaled half-extents.
        assert_eq!(q[0], [-960.0, -415.0, 0.0]);
        assert_eq!(q[1], [-960.0, -465.0, 0.0]);
        assert_eq!(q[2], [-760.0, -415.0, 0.0]);
        assert_eq!(q[3], [-760.0, -465.0, 0.0]);
    }

    #[test]
    fn text_and_image_conventions_agree() {
        // Text and image layers share one Y-up placement (the old Linux-port
        // "deliberate mirror" was a bug in that port's unfinished CText — see
        // scene_space_quad docs). For the same origin the text center-y must
        // EQUAL the image center-y (`renderer.rs::scene_space_quad`:
        // `cy = oy - sh/2`).
        let oy = 470.0;
        let q = scene_space_quad(0.0, oy, 10.0, 10.0, (1920, 1080));
        let text_cy = (q[0][1] + q[1][1]) / 2.0;
        let image_cy = oy - 540.0;
        assert_eq!(text_cy, image_cy);
    }

    #[test]
    fn glyph_top_lands_on_the_up_edge() {
        // The build_text UV order pairs v=0 (the bitmap's top glyph row) with
        // vertices 0 and 2 — they must be the higher-Y pair in kirie's Y-up
        // scene space, or the glyphs render upside down.
        let q = scene_space_quad(0.0, 0.0, 100.0, 40.0, (1920, 1080));
        assert!(q[0][1] > q[1][1], "TL (v=0) above BL (v=1)");
        assert!(q[2][1] > q[3][1], "TR (v=0) above BR (v=1)");
    }
}
