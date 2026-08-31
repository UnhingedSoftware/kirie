use std::collections::HashMap;

use kirie_audio::AudioSpectrum;
use kirie_scene::material::Material;
use kirie_scene::object::{AnimationTrack, ModelObject, Object};
use kirie_scene::resolve::AssetSource;
use kirie_shader::IncludeResolver;

use super::fbo::{FBO_FORMAT, Fbo};
use super::matrix::{self, Mat4};
use super::pipeline::{self, BuiltPass};
use super::renderer::{
    build_bind_group, create_buffer_init, create_ubo, is_scene_rt, resolve_params, tex_res,
};
use super::texture::TextureRegistry;
use super::uniforms::{Builtins, GlobalsLayout, pack_globals};

pub(super) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

fn padded_indices(indices: &[u16]) -> std::borrow::Cow<'_, [u16]> {
    if indices.len().is_multiple_of(2) {
        return std::borrow::Cow::Borrowed(indices);
    }
    let mut padded = indices.to_vec();
    padded.push(*indices.last().unwrap_or(&0));
    std::borrow::Cow::Owned(padded)
}

fn clamp_camera(fov: f32, near: f32, far: f32) -> (f32, f32, f32) {
    let fov = if (1.0..=170.0).contains(&fov) { fov } else { 50.0 };
    let near = if near > 0.0 { near } else { 0.1 };
    let far = if far > near { far } else { 10000.0 };
    (fov, near, far)
}

struct MeshGpu {
    pipeline: wgpu::RenderPipeline,
    g0_bind: wgpu::BindGroup,
    g1_bind: wgpu::BindGroup,
    vs_ubo: Option<wgpu::Buffer>,
    fs_ubo: Option<wgpu::Buffer>,
    vs_globals: GlobalsLayout,
    fs_globals: GlobalsLayout,
    vs_params: std::collections::BTreeMap<String, Vec<f32>>,
    fs_params: std::collections::BTreeMap<String, Vec<f32>>,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    tex_resolution: [[f32; 4]; 8],
}

impl ModelGpu {
    pub(super) fn set_origin(&mut self, v: [f32; 3]) {
        self.origin = v;
    }

    pub(super) fn set_scale(&mut self, v: [f32; 3]) {
        self.scale = v;
    }

    pub(super) fn set_angles(&mut self, v: [f32; 3]) {
        self.angles = v;
    }

    pub(super) fn has_animation(&self) -> bool {
        self.angles_animation.is_some()
    }
}

pub(super) struct ModelGpu {
    pub(super) id: i64,
    meshes: Vec<MeshGpu>,
    origin: [f32; 3],
    scale: [f32; 3],
    angles: [f32; 3],
    angles_animation: Option<AnimationTrack>,
    pub(super) visible: bool,
    pub(super) reads_scene: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_model(
    device: &wgpu::Device,
    object: &Object,
    model_object: &ModelObject,
    scene_size: (u32, u32),
    source: &dyn AssetSource,
    resolver: &dyn IncludeResolver,
    registry: &mut TextureRegistry,
    fbo_sampler: &wgpu::Sampler,
    scene_snapshot: &Fbo,
) -> Option<ModelGpu> {
    let bytes = source.load(&model_object.model)?;
    let model = match kirie_formats::model::Model::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(model = %model_object.model, error = %e, "model .mdl parse failed; skipped");
            return None;
        }
    };

    let mut reads_scene = false;
    let mut meshes = Vec::new();
    for (mi, mesh) in model.meshes.iter().enumerate() {
        let Some(mat_bytes) = source.load(&mesh.material_ref) else {
            tracing::debug!(material = %mesh.material_ref, "model material missing; mesh skipped");
            continue;
        };
        let Ok(mat_value) = serde_json::from_slice::<serde_json::Value>(&mat_bytes) else {
            tracing::debug!(material = %mesh.material_ref, "model material invalid JSON; mesh skipped");
            continue;
        };
        let material = Material::from_value(&mat_value);
        let Some(raw_pass) = material.passes.first().cloned() else {
            tracing::debug!(material = %mesh.material_ref, "model material has no pass; mesh skipped");
            continue;
        };

        let vs_name = format!("shaders/{}.vert", raw_pass.shader);
        let fs_name = format!("shaders/{}.frag", raw_pass.shader);
        let (Some(vs_bytes), Some(fs_bytes)) = (source.load(&vs_name), source.load(&fs_name)) else {
            tracing::debug!(shader = %raw_pass.shader, "model shader source missing; mesh skipped");
            continue;
        };
        let (Ok(vs_src), Ok(fs_src)) = (String::from_utf8(vs_bytes), String::from_utf8(fs_bytes)) else {
            continue;
        };
        let built = match pipeline::build_model_pass(
            device,
            FBO_FORMAT,
            DEPTH_FORMAT,
            &raw_pass,
            &vs_src,
            &fs_src,
            resolver,
        ) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(mesh = mi, shader = %raw_pass.shader, error = %e, "model mesh pipeline failed; skipped");
                continue;
            }
        };

        let vertex_buffer = create_buffer_init(
            device,
            "kirie-model-vb",
            &mesh.vertex_data,
            wgpu::BufferUsages::VERTEX,
        );
        let indices = padded_indices(&mesh.indices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&indices);
        let index_buffer =
            create_buffer_init(device, "kirie-model-ib", index_bytes, wgpu::BufferUsages::INDEX);
        let index_count = mesh.indices.len() as u32;

        let input = registry.white();

        let mut named: HashMap<&str, (&wgpu::TextureView, &wgpu::Sampler)> = HashMap::new();
        named.insert("_rt_FullFrameBuffer", (&scene_snapshot.view, fbo_sampler));
        named.insert("_rt_MipMappedFrameBuffer", (&scene_snapshot.view, fbo_sampler));
        if built
            .fs_samplers
            .iter()
            .chain(built.vs_samplers.iter())
            .any(|s| s.default_texture.as_deref().is_some_and(is_scene_rt))
            || raw_pass.textures.iter().flatten().any(|n| is_scene_rt(n))
        {
            reads_scene = true;
        }

        let vs_ubo = (!built.vs_globals.is_empty()).then(|| create_ubo(device, built.vs_globals.size));
        let fs_ubo = (!built.fs_globals.is_empty()).then(|| create_ubo(device, built.fs_globals.size));

        let g0_bind = build_bind_group(
            device,
            &built.g0_layout,
            vs_ubo.as_ref(),
            &built.g0_bindings,
            &built.vs_samplers,
            &input.view,
            &input.sampler,
            registry,
            source,
            &raw_pass,
            (&scene_snapshot.view, fbo_sampler),
            &named,
            false,
        );
        let g1_bind = build_bind_group(
            device,
            &built.g1_layout,
            fs_ubo.as_ref(),
            &built.g1_bindings,
            &built.fs_samplers,
            &input.view,
            &input.sampler,
            registry,
            source,
            &raw_pass,
            (&scene_snapshot.view, fbo_sampler),
            &named,
            false,
        );

        let tex_resolution =
            build_tex_resolution(&built, &raw_pass, scene_size, registry, source, input.as_ref());

        let vs_params = resolve_params(&built.vs_params, &raw_pass);
        let fs_params = resolve_params(&built.fs_params, &raw_pass);

        let BuiltPass {
            pipeline,
            vs_globals,
            fs_globals,
            ..
        } = built;

        meshes.push(MeshGpu {
            pipeline,
            g0_bind,
            g1_bind,
            vs_ubo,
            fs_ubo,
            vs_globals,
            fs_globals,
            vs_params,
            fs_params,
            vertex_buffer,
            index_buffer,
            index_count,
            tex_resolution,
        });
    }

    if meshes.is_empty() {
        return None;
    }

    let origin = object.base.origin.value;
    let scale = object.base.scale.value;
    let angles = object.base.angles.value;
    let angles_animation = object.base.angles_animation.clone();
    let visible = object.base.visible.value;
    tracing::debug!(
        id = object.base.id,
        model = %model_object.model,
        meshes = meshes.len(),
        "built 3D model object"
    );
    Some(ModelGpu {
        id: object.base.id,
        meshes,
        origin,
        scale,
        angles,
        angles_animation,
        visible,
        reads_scene,
    })
}

fn build_tex_resolution(
    built: &BuiltPass,
    pass: &kirie_scene::material::Pass,
    scene_size: (u32, u32),
    registry: &mut TextureRegistry,
    source: &dyn AssetSource,
    input: &super::texture::GpuTexture,
) -> [[f32; 4]; 8] {
    let scene_res = [
        scene_size.0 as f32,
        scene_size.1 as f32,
        scene_size.0 as f32,
        scene_size.1 as f32,
    ];
    let mut out = [scene_res; 8];
    out[0] = tex_res(input);
    for slot in &built.fs_samplers {
        let Some(i) = slot.slot else { continue };
        let i = i as usize;
        if i == 0 || i >= 8 {
            continue;
        }
        let name = pass
            .textures
            .get(i)
            .and_then(|s| s.clone())
            .or_else(|| slot.default_texture.clone());
        out[i] = match name {
            Some(n) if n.starts_with("_rt_") || n.starts_with("_alias_") => scene_res,
            Some(n) => tex_res(&registry.get(&n, source)),
            None => scene_res,
        };
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_model(
    encoder: &mut wgpu::CommandEncoder,
    queue: &wgpu::Queue,
    model: &ModelGpu,
    scene_view: &wgpu::TextureView,
    depth_view: &wgpu::TextureView,
    camera: &kirie_scene::scene::Camera,
    aspect: f32,
    ambient: [f32; 3],
    skylight: [f32; 3],
    time: f32,
    texel: [f32; 2],
    audio: Option<&AudioSpectrum>,
    scratch: &mut Vec<u8>,
    pointer: [f32; 2],
    pointer_last: [f32; 2],
) {
    let (fov, near, far) = clamp_camera(camera.fov.value, camera.nearz, camera.farz);
    let projection = matrix::perspective(fov.to_radians(), aspect, near, far);
    let view = matrix::look_at(camera.eye, camera.center, camera.up);
    let view_projection = matrix::mul(&projection, &view);
    let angles = match model
        .angles_animation
        .as_ref()
        .and_then(|t| t.sample(time).map(|off| (t, off)))
    {
        Some((t, off)) if t.relative => [
            model.angles[0] + off[0],
            model.angles[1] + off[1],
            model.angles[2] + off[2],
        ],
        Some((_, off)) => off,
        None => model.angles,
    };
    let model_matrix = compute_model_matrix(model.origin, angles, model.scale);
    let mvp = matrix::mul(&view_projection, &model_matrix);

    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("kirie-model-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: scene_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth_view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });

    for mesh in &model.meshes {
        let builtins = Builtins {
            time,
            daytime: 0.0,
            brightness: 1.0,
            alpha: 1.0,
            color: [1.0, 1.0, 1.0, 1.0],
            ambient,
            skylight,
            pointer,
            pointer_last,
            texel_size: texel,
            mvp,
            mvp_inverse: None,
            model: model_matrix,
            view_projection,
            eye: camera.eye,
            texture0_translation: [0.0, 0.0],
            texture0_rotation: [0.0, 0.0, 0.0, 0.0],
            texture_resolution: mesh.tex_resolution,
            audio16: audio.map_or([0.0; 16], |a| a.audio16),
            audio32: audio.map_or([0.0; 32], |a| a.audio32),
            audio64: audio.map_or([0.0; 64], |a| a.audio64),
        };
        if let Some(ubo) = &mesh.vs_ubo {
            pack_globals(scratch, &mesh.vs_globals, &builtins, &mesh.vs_params);
            queue.write_buffer(ubo, 0, scratch);
        }
        if let Some(ubo) = &mesh.fs_ubo {
            pack_globals(scratch, &mesh.fs_globals, &builtins, &mesh.fs_params);
            queue.write_buffer(ubo, 0, scratch);
        }
        rp.set_pipeline(&mesh.pipeline);
        rp.set_bind_group(0, &mesh.g0_bind, &[]);
        rp.set_bind_group(1, &mesh.g1_bind, &[]);
        rp.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
        rp.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        rp.draw_indexed(0..mesh.index_count, 0, 0..1);
    }
}

fn compute_model_matrix(origin: [f32; 3], angles: [f32; 3], scale: [f32; 3]) -> Mat4 {
    let mut m = matrix::translation(origin);
    m = matrix::mul(&m, &matrix::rotation_z(angles[2]));
    m = matrix::mul(&m, &matrix::rotation_y(angles[1]));
    m = matrix::mul(&m, &matrix::rotation_x(angles[0]));
    matrix::mul(&m, &matrix::scale(scale))
}

pub(super) fn create_depth_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let (width, height) = super::fbo::fit_within(width, height, device.limits().max_texture_dimension_2d);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-model-depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use super::padded_indices;

    #[test]
    fn an_even_index_run_is_left_alone() {
        let indices = [0u16, 1, 2, 3];
        assert!(matches!(padded_indices(&indices), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn an_odd_index_run_is_padded_to_a_whole_word() {
        let padded = padded_indices(&[0u16, 1, 2]);
        assert_eq!(padded.len(), 4);
        assert_eq!(&padded[..3], &[0, 1, 2]);
        assert_eq!(padded[3], 2, "the pad repeats a vertex, drawing nothing new");
    }

    #[test]
    fn an_empty_run_stays_empty() {
        assert!(padded_indices(&[]).is_empty());
    }

    use super::*;

    #[test]
    fn camera_clamps_match_reference() {
        assert_eq!(clamp_camera(50.0, 0.01, 11.0), (50.0, 0.01, 11.0));
        assert_eq!(clamp_camera(0.0, 0.0, -1.0), (50.0, 0.1, 10000.0));
        assert_eq!(clamp_camera(200.0, 0.01, 0.005), (50.0, 0.01, 10000.0));
    }

    #[test]
    fn model_matrix_places_origin() {
        let m = compute_model_matrix([1.0, -2.0, 3.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert_eq!([m[12], m[13], m[14]], [1.0, -2.0, 3.0]);
    }

    #[test]
    #[ignore = "heavy GPU + corpus diagnostic; run manually with --ignored"]
    fn model_only_on_magenta() {
        use kirie_scene::object::ObjectKind;
        use kirie_scene::resolve::AssetSource;
        use kirie_scene::{PropertyBag, Scene, SceneModel};
        use kirie_shader::IncludeResolver;

        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_test_writer()
            .try_init();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            eprintln!("skip: no adapter");
            return;
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("model-only-diag"),
            ..wgpu::DeviceDescriptor::default()
        })) else {
            eprintln!("skip: no device");
            return;
        };

        let scene_dir =
            std::path::Path::new("/home/aiko/.steam/steam/steamapps/workshop/content/431960/3047596375");
        let assets = std::path::PathBuf::from(
            "/home/aiko/.local/share/Steam/steamapps/common/wallpaper_engine/assets",
        );
        let pkg = match kirie_formats::pkg::OwnedPkg::from_path(scene_dir.join("scene.pkg")) {
            Ok(p) => p,
            Err(_) => {
                eprintln!("skip: corpus absent");
                return;
            }
        };

        struct Src {
            pkg: kirie_formats::pkg::OwnedPkg,
            assets: std::path::PathBuf,
        }
        impl AssetSource for Src {
            fn load(&self, path: &str) -> Option<Vec<u8>> {
                if let Ok(b) = self.pkg.read_name(path.as_bytes()) {
                    return Some(b.to_vec());
                }
                std::fs::read(self.assets.join(path)).ok()
            }
        }
        struct Inc<'a>(&'a dyn AssetSource);
        impl IncludeResolver for Inc<'_> {
            fn resolve(&self, name: &str) -> Option<String> {
                String::from_utf8(self.0.load(&format!("shaders/{name}"))?).ok()
            }
        }

        let scene_bytes = pkg.read_name(b"scene.json").expect("scene.json").to_vec();
        let scene = Scene::from_slice(&scene_bytes).expect("parse scene");
        let model = SceneModel::resolve(scene, &PropertyBag::default());
        let (obj, mo) = model
            .scene
            .objects
            .iter()
            .find_map(|o| match &o.kind {
                ObjectKind::Model(m) => Some((o, m)),
                _ => None,
            })
            .expect("a model object");

        let src = Src { pkg, assets };
        let resolver = Inc(&src);
        let mut registry = TextureRegistry::new(&device, &queue);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let (w, h) = (636u32, 692u32);
        let snapshot = Fbo::new(&device, "diag-snap", w, h);
        let mg = build_model(
            &device,
            obj,
            mo,
            (w, h),
            &src,
            &resolver,
            &mut registry,
            &sampler,
            &snapshot,
        )
        .expect("build_model returned None");
        eprintln!("model built: {} mesh(es)", mg.meshes.len());

        let color = Fbo::new(&device, "diag-color", w, h);
        let depth = create_depth_texture(&device, w, h);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _c = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("diag-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 0.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        let aspect = w as f32 / h as f32;
        draw_model(
            &mut enc,
            &queue,
            &mg,
            &color.view,
            &depth,
            &model.scene.camera,
            aspect,
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            0.0,
            [1.0 / w as f32, 1.0 / h as f32],
            None,
            &mut Vec::new(),
            [0.5, 0.5],
            [0.5, 0.5],
        );
        queue.submit(Some(enc.finish()));

        let padded = (w * 8).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("diag-rb"),
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.expect("map"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let mapped = buffer.get_mapped_range(..).expect("range");

        let f16 = |bytes: &[u8]| -> f32 {
            let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
            let sign = (bits >> 15) & 1;
            let exp = (bits >> 10) & 0x1f;
            let man = bits & 0x3ff;
            let val = if exp == 0 {
                f32::from(man) * 2f32.powi(-24)
            } else if exp == 31 {
                f32::INFINITY
            } else {
                (1.0 + f32::from(man) / 1024.0) * 2f32.powi(i32::from(exp) - 15)
            };
            if sign == 1 { -val } else { val }
        };

        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let off = (y * padded + x * 8) as usize;
                let px = ((y * w + x) * 4) as usize;
                for c in 0..3 {
                    let v = f16(&mapped[off + c * 2..]).clamp(0.0, 1.0);
                    rgba[px + c] = (v * 255.0) as u8;
                }
                rgba[px + 3] = 255;
            }
        }
        let png_path = std::env::temp_dir().join("kirie-model-only.png");
        let _ = image::save_buffer(&png_path, &rgba, w, h, image::ColorType::Rgba8);
        eprintln!("wrote {}", png_path.display());

        let (mut minx, mut miny, mut maxx, mut maxy) = (w, h, 0u32, 0u32);
        let mut drew = 0u64;
        let cell = w / 48;
        let mut grid = String::new();
        for gy in 0..48u32 {
            for gx in 0..48u32 {
                let px = (gx * cell + cell / 2).min(w - 1);
                let py = (gy * cell + cell / 2).min(h - 1);
                let off = (py * padded + px * 8) as usize;
                let (r, g, b) = (
                    f16(&mapped[off..]),
                    f16(&mapped[off + 2..]),
                    f16(&mapped[off + 4..]),
                );
                let diff = (r - 1.0).abs() + g.abs() + (b - 1.0).abs();
                grid.push(if diff > 0.1 { '#' } else { ' ' });
            }
            grid.push('\n');
        }
        for y in 0..h {
            for x in 0..w {
                let off = (y * padded + x * 8) as usize;
                let (r, g, b) = (
                    f16(&mapped[off..]),
                    f16(&mapped[off + 2..]),
                    f16(&mapped[off + 4..]),
                );
                let diff = (r - 1.0).abs() + g.abs() + (b - 1.0).abs();
                if diff > 0.1 {
                    drew += 1;
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x);
                    maxy = maxy.max(y);
                }
            }
        }
        eprintln!("model-drawn pixels (differ from magenta): {drew} / {}", w * h);
        if drew > 0 {
            eprintln!("bbox: x[{minx}..{maxx}] y[{miny}..{maxy}]  (image is {w}x{h})");
        }
        eprintln!("{grid}");
        drop(mapped);
        buffer.unmap();
    }

    #[test]
    fn model_matrix_scales_then_translates() {
        let m = compute_model_matrix([0.0, -0.84, 0.0], [0.0, 0.0, 0.0], [0.003, 0.003, 0.003]);
        assert!((m[0] - 0.003).abs() < 1e-6);
        assert_eq!([m[12], m[13], m[14]], [0.0, -0.84, 0.0]);
    }
}
