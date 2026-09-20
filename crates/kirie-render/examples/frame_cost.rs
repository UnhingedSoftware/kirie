//! Renders a synthetic scene headlessly and reports what one frame costs.
//!
//! The scene is built in memory so the benchmark needs no Wallpaper Engine
//! content: a stack of full-screen image layers, some of them carrying an
//! effect so the pass graph has offscreen targets and scene reads in it, the
//! way a real wallpaper does.
//!
//! ```text
//! cargo run --release --example frame_cost -- [layers] [effect_every] [composite_every] [frames]
//! ```

use std::collections::HashMap;

use kirie_platform::{RenderTarget, Renderer, SurfaceSize};
use kirie_render::{ClampMode, ScalingMode, SceneOptions, SceneRenderer};
use kirie_scene::resolve::AssetSource;
use kirie_scene::{PropertyBag, Scene, SceneModel};

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
uniform vec4 g_Texture0Resolution;
uniform float g_Alpha;
uniform vec3 g_Color;
varying vec2 v_TexCoord;
void main() {
    vec4 albedo = texture(g_Texture0, v_TexCoord);
    gl_FragColor = vec4(albedo.rgb * g_Color, albedo.a * g_Alpha);
}
"#;

// An effect pass that moves with time, so it counts as animated the way a real
// effect does and cannot be folded away as static content.
const EFFECT_FRAG: &str = r#"
uniform sampler2D g_Texture0;
uniform float g_Time;
varying vec2 v_TexCoord;
void main() {
    vec2 uv = v_TexCoord + vec2(sin(g_Time) * 0.004, cos(g_Time) * 0.004);
    vec4 c = texture(g_Texture0, uv);
    gl_FragColor = vec4(c.rgb * 1.02, c.a);
}
"#;

struct Memory(HashMap<String, Vec<u8>>);

impl AssetSource for Memory {
    fn load(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

fn build(layers: usize, effect_every: usize, composite_every: usize) -> (String, Memory) {
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    files.insert("shaders/bench.vert".into(), VERT.into());
    files.insert("shaders/bench.frag".into(), FRAG.into());
    files.insert("shaders/benchfx.vert".into(), VERT.into());
    files.insert("shaders/benchfx.frag".into(), EFFECT_FRAG.into());
    files.insert(
        "materials/bench.json".into(),
        br#"{"passes":[{"shader":"bench","blending":"translucent","textures":["bench_albedo"]}]}"#.to_vec(),
    );
    files.insert(
        "models/bench.json".into(),
        br#"{"material":"materials/bench.json"}"#.to_vec(),
    );
    files.insert(
        "materials/benchcomposite.json".into(),
        br#"{"passes":[{"shader":"bench","blending":"normal","textures":["_rt_FullFrameBuffer"]}]}"#
            .to_vec(),
    );
    files.insert(
        "models/benchcomposite.json".into(),
        br#"{"material":"materials/benchcomposite.json"}"#.to_vec(),
    );
    files.insert(
        "materials/benchfx.json".into(),
        br#"{"passes":[{"shader":"benchfx","textures":[null]}]}"#.to_vec(),
    );
    files.insert(
        "effects/benchfx.json".into(),
        br#"{"name":"benchfx","passes":[{"material":"materials/benchfx.json"}]}"#.to_vec(),
    );

    let mut objects = Vec::new();
    for i in 0..layers {
        let effects = if effect_every > 0 && i % effect_every == 0 {
            r#""effects":[{"file":"effects/benchfx.json","visible":true}],"#
        } else {
            ""
        };
        let model = if composite_every > 0 && i > 0 && i % composite_every == 0 {
            "models/benchcomposite.json"
        } else {
            "models/bench.json"
        };
        objects.push(format!(
            r#"{{"id":{i},"name":"layer{i}","image":"{model}","visible":true,
                "origin":"0 0 0","scale":"1 1 1","angles":"0 0 0","alpha":1.0,
                "color":"1 1 1","size":"1920 1080",{effects}
                "alignment":"center"}}"#
        ));
    }

    let scene = format!(
        r#"{{"camera":{{"eye":"0 0 100","center":"0 0 0","up":"0 1 0"}},
            "general":{{"orthogonalprojection":{{"width":1920,"height":1080}},
                        "clearcolor":"0 0 0","bloom":false}},
            "objects":[{}]}}"#,
        objects.join(",")
    );
    (scene, Memory(files))
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, String)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let info = adapter.get_info();
    let name = format!("{} ({:?})", info.name, info.backend);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("frame-cost"),
        ..wgpu::DeviceDescriptor::default()
    }))
    .ok()?;
    Some((device, queue, name))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let layers: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(24);
    let effect_every: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(4);
    let composite_every: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(6);
    let frames: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(120);
    let (width, height) = (1920u32, 1080u32);

    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    let Some((device, queue, adapter)) = gpu() else {
        eprintln!("no wgpu adapter; nothing to measure");
        std::process::exit(1);
    };

    let (scene_json, source) = build(layers, effect_every, composite_every);
    let scene = Scene::from_slice(scene_json.as_bytes()).expect("synthetic scene.json parses");
    let bag = PropertyBag::default();
    let mut model = SceneModel::resolve(scene, &bag);
    for problem in model.load_assets(&source, &bag) {
        eprintln!("asset: {} — {}", problem.path, problem.reason);
    }

    let target = RenderTarget {
        device: &device,
        queue: &queue,
        format: wgpu::TextureFormat::Rgba8Unorm,
        output_name: "frame-cost",
        size: (width, height),
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
            disable_parallax: false,
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
    .expect("the synthetic scene builds");

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("frame-cost-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let size = SurfaceSize { width, height };

    // Warm up: first frames build lazily and are not representative.
    for _ in 0..8 {
        renderer.render(&view, size, 1.0 / 30.0);
    }
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");

    kirie_render::frame_cost::reset();
    let mut cpu = Vec::with_capacity(frames);
    let started = std::time::Instant::now();
    for _ in 0..frames {
        let t = std::time::Instant::now();
        renderer.render(&view, size, 1.0 / 30.0);
        cpu.push(t.elapsed().as_secs_f64() * 1e3);
    }
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let wall = started.elapsed().as_secs_f64() * 1e3 / frames as f64;

    cpu.sort_by(f64::total_cmp);
    let median = cpu[cpu.len() / 2];
    let cost = kirie_render::frame_cost::per_frame(frames as u64);

    println!("adapter        {adapter}");
    println!(
        "scene          {layers} layers, an effect on every {effect_every}, a scene-reading layer on every {composite_every}, {width}x{height}"
    );
    println!("frames         {frames}");
    println!("record median  {median:.3} ms/frame (CPU time inside render())");
    println!("wall           {wall:.3} ms/frame (record + submit, GPU pipelined)");
    println!("render passes  {:.1} /frame", cost.render_passes);
    println!("draw calls     {:.1} /frame", cost.draws);
    println!("texture copies {:.1} /frame ({:.2} MiB)", cost.texture_copies, cost.copy_mib);
    println!("buffer writes  {:.1} /frame ({:.1} KiB)", cost.buffer_writes, cost.buffer_kib);
    if cost.snapshots_skipped > 0.0 {
        println!("copies skipped {:.1} /frame (scene unchanged since the last one)", cost.snapshots_skipped);
    }
    if cost.buffer_writes_skipped > 0.0 {
        println!("writes skipped {:.1} /frame (bytes already in the buffer)", cost.buffer_writes_skipped);
    }

    // A fingerprint of the last frame, so a change meant to cost less can be
    // checked for drawing the same picture.
    println!("frame digest   {}", digest(&device, &queue, &texture, width, height));
}

/// Reads the rendered frame back and hashes it.
fn digest(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> String {
    let padded_row = (width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("frame-cost-readback"),
        size: u64::from(padded_row * height),
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
                bytes_per_row: Some(padded_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.expect("map"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let mapped = buffer.get_mapped_range(..).expect("mapped range");
    let mut hasher = blake3::Hasher::new();
    for row in 0..height {
        let start = (row * padded_row) as usize;
        hasher.update(&mapped[start..start + (width * 4) as usize]);
    }
    let hash = hasher.finalize().to_hex().to_string();
    drop(mapped);
    buffer.unmap();
    hash[..16].to_string()
}
