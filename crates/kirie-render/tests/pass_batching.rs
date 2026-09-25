//! Sharing one render pass between the layers that composite into the scene
//! must not change a single pixel. This renders the same scene both ways and
//! compares the frames byte for byte.

use std::collections::HashMap;

use kirie_platform::{RenderTarget, Renderer, SurfaceSize};
use kirie_render::{ClampMode, ScalingMode, SceneOptions, SceneRenderer};
use kirie_scene::resolve::AssetSource;
use kirie_scene::{PropertyBag, Scene, SceneModel};

const W: u32 = 160;
const H: u32 = 90;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const VERT: &str = r#"
attribute vec3 a_Position;
attribute vec2 a_TexCoord;
uniform mat4 g_ModelViewProjectionMatrix;
varying vec2 v_TexCoord;
void main() {
    gl_Position = g_ModelViewProjectionMatrix * vec4(a_Position, 1.0);
    v_TexCoord = a_TexCoord;
}
"#;

const FRAG: &str = r#"
uniform sampler2D g_Texture0;
uniform float g_Time;
uniform float g_Alpha;
uniform vec3 g_Color;
varying vec2 v_TexCoord;
void main() {
    vec4 albedo = texture(g_Texture0, v_TexCoord + vec2(sin(g_Time) * 0.01, 0.0));
    gl_FragColor = vec4(albedo.rgb * g_Color, albedo.a * g_Alpha);
}
"#;

struct Memory(HashMap<String, Vec<u8>>);

impl AssetSource for Memory {
    fn load(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

/// Plain layers plus one that samples the scene behind it, so both the shared
/// pass and the scene-snapshot copy are exercised.
fn scene() -> (Scene, Memory) {
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    files.insert("shaders/t.vert".into(), VERT.into());
    files.insert("shaders/t.frag".into(), FRAG.into());
    files.insert(
        "materials/plain.json".into(),
        br#"{"passes":[{"shader":"t","blending":"translucent","textures":["albedo"]}]}"#.to_vec(),
    );
    files.insert(
        "models/plain.json".into(),
        br#"{"material":"materials/plain.json"}"#.to_vec(),
    );
    files.insert(
        "materials/composite.json".into(),
        br#"{"passes":[{"shader":"t","blending":"normal","textures":["_rt_FullFrameBuffer"]}]}"#.to_vec(),
    );
    files.insert(
        "models/composite.json".into(),
        br#"{"material":"materials/composite.json"}"#.to_vec(),
    );

    let objects: Vec<String> = (0..5)
        .map(|i| {
            let model = if i == 2 || i == 4 {
                "models/composite.json"
            } else {
                "models/plain.json"
            };
            format!(
                r#"{{"id":{i},"name":"l{i}","image":"{model}","visible":true,"origin":"0 0 0",
                    "scale":"1 1 1","angles":"0 0 0","alpha":0.8,"color":"1 1 1",
                    "size":"{W} {H}","alignment":"center"}}"#
            )
        })
        .collect();
    let json = format!(
        r#"{{"camera":{{"eye":"0 0 100","center":"0 0 0","up":"0 1 0"}},
            "general":{{"orthogonalprojection":{{"width":{W},"height":{H}}},"clearcolor":"0 0 0"}},
            "objects":[{}]}}"#,
        objects.join(",")
    );
    (
        Scene::from_slice(json.as_bytes()).expect("the test scene parses"),
        Memory(files),
    )
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("skipping: no adapter ({err})");
            return None;
        }
    };
    match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("pass-batching"),
        ..wgpu::DeviceDescriptor::default()
    })) {
        Ok(pair) => Some(pair),
        Err(err) => {
            eprintln!("skipping: no device ({err})");
            None
        }
    }
}

fn render_frames(device: &wgpu::Device, queue: &wgpu::Queue, frames: usize) -> Vec<u8> {
    let (scene, source) = scene();
    let bag = PropertyBag::default();
    let mut model = SceneModel::resolve(scene, &bag);
    let problems = model.load_assets(&source, &bag);
    assert!(problems.is_empty(), "test assets all resolve: {problems:?}");

    let target = RenderTarget {
        device,
        queue,
        format: FORMAT,
        output_name: "pass-batching",
        size: (W, H),
        position: (0, 0),
    };
    let mut renderer = SceneRenderer::new(
        &target,
        &model,
        &source,
        SceneOptions {
            render_scale: 1.0,
            scaling: ScalingMode::Fill,
            clamp: ClampMode::Clamp,
            disable_parallax: true,
            fit_render_to_output: false,
            only_objects: Vec::new(),
            skip_objects: Vec::new(),
            skip_effects: Vec::new(),
            base_only: false,
            no_solid_final: false,
            pass_log: false,
        },
        None,
        &[],
    )
    .expect("the test scene builds");

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pass-batching-target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    for _ in 0..frames {
        renderer.render(&view, SurfaceSize { width: W, height: H }, 1.0 / 30.0);
    }
    read_back(device, queue, &texture)
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let padded = (W * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pass-batching-readback"),
        size: u64::from(padded * H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
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
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.expect("map"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let mapped = buffer.get_mapped_range(..).expect("mapped range");
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H {
        let start = (row * padded) as usize;
        pixels.extend_from_slice(&mapped[start..start + (W * 4) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    pixels
}

#[test]
fn sharing_the_scene_pass_draws_the_same_frame() {
    let Some((device, queue)) = gpu() else { return };

    kirie_render::scene::encoder::set_unbatched_passes(true);
    let apart = render_frames(&device, &queue, 12);
    kirie_render::scene::encoder::set_unbatched_passes(false);
    let together = render_frames(&device, &queue, 12);

    assert!(
        apart.iter().any(|&b| b != 0),
        "the test scene drew something; otherwise this compares two black frames"
    );
    assert_eq!(
        apart, together,
        "a frame recorded in one shared render pass differs from one recorded a pass at a time"
    );
}
