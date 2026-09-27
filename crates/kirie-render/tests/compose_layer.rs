//! A compose layer with "copy background" off draws its effects on a
//! transparent canvas. Wallpapers put audio bars on such layers and scroll or
//! warp them; drawing on a copy of the scene instead showed a moving square of
//! background.

use std::collections::HashMap;

use kirie_platform::{RenderTarget, Renderer, SurfaceSize};
use kirie_render::{ClampMode, ScalingMode, SceneOptions, SceneRenderer};
use kirie_scene::resolve::AssetSource;
use kirie_scene::{PropertyBag, Scene, SceneModel};

const W: u32 = 64;
const H: u32 = 64;
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

const PASS_FRAG: &str = r#"
uniform sampler2D g_Texture0;
varying vec2 v_TexCoord;
void main() {
    gl_FragColor = texture(g_Texture0, v_TexCoord);
}
"#;

/// Drops red: a copy of the red scene turns black, an empty canvas stays empty.
const TINT_FRAG: &str = r#"
uniform sampler2D g_Texture0;
varying vec2 v_TexCoord;
void main() {
    vec4 c = texture(g_Texture0, v_TexCoord);
    gl_FragColor = vec4(c.rgb * vec3(0.0, 1.0, 1.0), c.a);
}
"#;

struct Memory(HashMap<String, Vec<u8>>);

impl AssetSource for Memory {
    fn load(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

fn scene(copybackground: bool) -> (Scene, Memory) {
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    files.insert("shaders/util/composelayer.vert".into(), VERT.into());
    files.insert("shaders/util/composelayer.frag".into(), PASS_FRAG.into());
    files.insert("shaders/tint.vert".into(), VERT.into());
    files.insert("shaders/tint.frag".into(), TINT_FRAG.into());
    files.insert(
        "materials/util/composelayer.json".into(),
        br#"{"passes":[{"shader":"util/composelayer","blending":"translucent","textures":["_rt_FullFrameBuffer"]}]}"#
            .to_vec(),
    );
    files.insert(
        "models/util/composelayer.json".into(),
        br#"{"material":"materials/util/composelayer.json"}"#.to_vec(),
    );
    files.insert(
        "materials/tint.json".into(),
        br#"{"passes":[{"shader":"tint","blending":"normal"}]}"#.to_vec(),
    );
    files.insert(
        "effects/tint/effect.json".into(),
        br#"{"passes":[{"material":"materials/tint.json"}]}"#.to_vec(),
    );
    let json = format!(
        r#"{{"camera":{{"eye":"0 0 100","center":"0 0 0","up":"0 1 0"}},
            "general":{{"orthogonalprojection":{{"width":{W},"height":{H}}},"clearcolor":"1.0 0.0 0.0"}},
            "objects":[{{"id":1,"name":"bars","image":"models/util/composelayer.json","visible":true,
                "origin":"{cx} {cy} 0","scale":"1 1 1","angles":"0 0 0","size":"{W} {H}",
                "copybackground":{copybackground},
                "effects":[{{"file":"effects/tint/effect.json","id":2,"name":"","visible":true,
                              "passes":[{{"id":3}}]}}]}}]}}"#,
        cx = W / 2,
        cy = H / 2,
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
        label: Some("compose-layer"),
        ..wgpu::DeviceDescriptor::default()
    })) {
        Ok(pair) => Some(pair),
        Err(err) => {
            eprintln!("skipping: no device ({err})");
            None
        }
    }
}

fn centre_pixel(device: &wgpu::Device, queue: &wgpu::Queue, copybackground: bool) -> [u8; 4] {
    let (scene, source) = scene(copybackground);
    let bag = PropertyBag::default();
    let mut model = SceneModel::resolve(scene, &bag);
    let problems = model.load_assets(&source, &bag);
    assert!(problems.is_empty(), "test assets all resolve: {problems:?}");

    let target = RenderTarget {
        device,
        queue,
        format: FORMAT,
        output_name: "compose-layer",
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
        label: Some("compose-layer-target"),
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
    renderer.render(&view, SurfaceSize { width: W, height: H }, 1.0 / 30.0);
    let pixels = read_back(device, queue, &texture);
    let at = ((H / 2 * W + W / 2) * 4) as usize;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let padded = (W * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("compose-layer-readback"),
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
fn a_compose_layer_without_copy_background_starts_empty() {
    let Some((device, queue)) = gpu() else { return };

    let copied = centre_pixel(&device, &queue, true);
    assert!(
        copied[0] < 16,
        "the tint ran on a copy of the red scene: {copied:?}"
    );

    let empty = centre_pixel(&device, &queue, false);
    assert!(
        empty[0] > 240 && empty[1] < 16 && empty[2] < 16,
        "the scene behind an empty compose layer shows through untouched: {empty:?}"
    );
}
