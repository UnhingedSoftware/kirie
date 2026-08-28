use super::fbo::{FBO_FORMAT, Fbo};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrightParams {
    texel: [f32; 2],
    strength: f32,
    threshold: f32,
    tint: [f32; 3],
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    step: [f32; 2],
    _pad: [f32; 2],
}

pub(crate) struct Bloom {
    quarter: Fbo,
    eighth: Fbo,
    bloom: Fbo,
    bright_pipeline: wgpu::RenderPipeline,
    blur_pipeline: wgpu::RenderPipeline,
    combine_pipeline: wgpu::RenderPipeline,
    bright_bind: wgpu::BindGroup,
    blur_x_bind: wgpu::BindGroup,
    blur_y_bind: wgpu::BindGroup,
    combine_bind: wgpu::BindGroup,
    bright_ub: wgpu::Buffer,
    texel: [f32; 2],
}

impl Bloom {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        proj_w: u32,
        proj_h: u32,
        scene_view: &wgpu::TextureView,
        snapshot_view: &wgpu::TextureView,
        strength: f32,
        threshold: f32,
    ) -> Self {
        const BLOOM_RT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
        let quarter = Fbo::with_format(
            device,
            "kirie-bloom-quarter",
            proj_w / 4,
            proj_h / 4,
            BLOOM_RT_FORMAT,
        );
        let eighth = Fbo::with_format(
            device,
            "kirie-bloom-eighth",
            proj_w / 8,
            proj_h / 8,
            BLOOM_RT_FORMAT,
        );
        let bloom = Fbo::with_format(device, "kirie-bloom", proj_w / 8, proj_h / 8, BLOOM_RT_FORMAT);

        let texel = [1.0 / proj_w.max(1) as f32, 1.0 / proj_h.max(1) as f32];
        let bright_ub = create_uniform(
            device,
            queue,
            "kirie-bloom-bright-ub",
            bytemuck::bytes_of(&BrightParams {
                texel,
                strength,
                threshold,
                tint: [1.0, 1.0, 1.0],
                _pad: 0.0,
            }),
        );
        let blur_x_ub = create_uniform(
            device,
            queue,
            "kirie-bloom-blurx-ub",
            bytemuck::bytes_of(&BlurParams {
                step: [texel[0] * 8.0, 0.0],
                _pad: [0.0, 0.0],
            }),
        );
        let blur_y_ub = create_uniform(
            device,
            queue,
            "kirie-bloom-blury-ub",
            bytemuck::bytes_of(&BlurParams {
                step: [0.0, texel[1] * 8.0],
                _pad: [0.0, 0.0],
            }),
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kirie-bloom-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let mk_mod = |label: &str, src: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        };
        let bright_mod = mk_mod("kirie-bloom-bright", SHADER_BRIGHT);
        let blur_mod = mk_mod("kirie-bloom-blur", SHADER_BLUR);
        let combine_mod = mk_mod("kirie-bloom-combine", SHADER_COMBINE);

        let tex_ub_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kirie-bloom-tex-ub-layout"),
            entries: &[sampler_entry(0), texture_entry(1), uniform_entry(2)],
        });
        let two_tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kirie-bloom-two-tex-layout"),
            entries: &[sampler_entry(0), texture_entry(1), texture_entry(2)],
        });

        let bright_pipeline = make_pipeline(device, &bright_mod, &tex_ub_layout, BLOOM_RT_FORMAT);
        let blur_pipeline = make_pipeline(device, &blur_mod, &tex_ub_layout, BLOOM_RT_FORMAT);
        let combine_pipeline = make_pipeline(device, &combine_mod, &two_tex_layout, FBO_FORMAT);

        let bright_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-bloom-bright-bind"),
            layout: &tex_ub_layout,
            entries: &[
                bind_sampler(0, &sampler),
                bind_texture(1, scene_view),
                bind_buffer(2, &bright_ub),
            ],
        });
        let blur_x_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-bloom-blurx-bind"),
            layout: &tex_ub_layout,
            entries: &[
                bind_sampler(0, &sampler),
                bind_texture(1, &quarter.view),
                bind_buffer(2, &blur_x_ub),
            ],
        });
        let blur_y_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-bloom-blury-bind"),
            layout: &tex_ub_layout,
            entries: &[
                bind_sampler(0, &sampler),
                bind_texture(1, &eighth.view),
                bind_buffer(2, &blur_y_ub),
            ],
        });
        let combine_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kirie-bloom-combine-bind"),
            layout: &two_tex_layout,
            entries: &[
                bind_sampler(0, &sampler),
                bind_texture(1, snapshot_view),
                bind_texture(2, &bloom.view),
            ],
        });

        Bloom {
            quarter,
            eighth,
            bloom,
            bright_pipeline,
            blur_pipeline,
            combine_pipeline,
            bright_bind,
            blur_x_bind,
            blur_y_bind,
            combine_bind,
            bright_ub,
            texel,
        }
    }

    pub(crate) fn set_params(&self, queue: &wgpu::Queue, strength: f32, threshold: f32) {
        queue.write_buffer(
            &self.bright_ub,
            0,
            bytemuck::bytes_of(&BrightParams {
                texel: self.texel,
                strength,
                threshold,
                tint: [1.0, 1.0, 1.0],
                _pad: 0.0,
            }),
        );
    }

    pub(crate) fn run(&self, encoder: &mut wgpu::CommandEncoder, scene_fbo: &Fbo, scene_snapshot: &Fbo) {
        self.pass(
            encoder,
            "bright",
            &self.bright_pipeline,
            &self.bright_bind,
            &self.quarter.view,
        );
        self.pass(
            encoder,
            "blurx",
            &self.blur_pipeline,
            &self.blur_x_bind,
            &self.eighth.view,
        );
        self.pass(
            encoder,
            "blury",
            &self.blur_pipeline,
            &self.blur_y_bind,
            &self.bloom.view,
        );
        encoder.copy_texture_to_texture(
            scene_fbo.texture.as_image_copy(),
            scene_snapshot.texture.as_image_copy(),
            wgpu::Extent3d {
                width: scene_fbo.width,
                height: scene_fbo.height,
                depth_or_array_layers: 1,
            },
        );
        self.pass(
            encoder,
            "combine",
            &self.combine_pipeline,
            &self.combine_bind,
            &scene_fbo.view,
        );
    }

    fn pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        pipeline: &wgpu::RenderPipeline,
        bind: &wgpu::BindGroup,
        target: &wgpu::TextureView,
    ) {
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(pipeline);
        rp.set_bind_group(0, bind, &[]);
        rp.draw(0..3, 0..1);
    }
}

fn create_uniform(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, data: &[u8]) -> wgpu::Buffer {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: data.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, data);
    buf
}

fn make_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    bind_layout: &wgpu::BindGroupLayout,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kirie-bloom-pipeline-layout"),
        bind_group_layouts: &[Some(bind_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-bloom-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn bind_sampler(binding: u32, s: &wgpu::Sampler) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Sampler(s),
    }
}
fn bind_texture(binding: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(v),
    }
}
fn bind_buffer(binding: u32, b: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: b.as_entire_binding(),
    }
}

const SHADER_BRIGHT: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VOut {
    var o: VOut;
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    o.uv = uv;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return o;
}
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var tex0: texture_2d<f32>;
struct BrightParams { texel: vec2<f32>, strength: f32, threshold: f32, tint: vec3<f32>, pad: f32 };
@group(0) @binding(2) var<uniform> bright: BrightParams;
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let t = bright.texel;
    var albedo = textureSample(tex0, samp, i.uv - t).rgb
               + textureSample(tex0, samp, i.uv + t).rgb
               + textureSample(tex0, samp, i.uv + vec2<f32>(-t.x, t.y)).rgb
               + textureSample(tex0, samp, i.uv + vec2<f32>(t.x, -t.y)).rgb;
    albedo = albedo * 0.25;
    let scale = max(max(albedo.x, albedo.y), albedo.z);
    albedo = albedo * clamp(scale - bright.threshold, 0.0, 1.0);
    let grayscale = dot(vec3<f32>(0.2989, 0.5870, 0.1140), albedo);
    let sat = 1.0;
    albedo = -grayscale * sat + albedo * (1.0 + sat);
    return vec4<f32>(max(vec3<f32>(0.0), albedo * bright.strength * bright.tint), 1.0);
}
"#;

const SHADER_BLUR: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VOut {
    var o: VOut;
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    o.uv = uv;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return o;
}
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var tex0: texture_2d<f32>;
struct BlurParams { step: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(2) var<uniform> blur: BlurParams;
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    var w = array<f32, 13>(
        0.006299, 0.017298, 0.039533, 0.075189, 0.119007, 0.156756, 0.171834,
        0.156756, 0.119007, 0.075189, 0.039533, 0.017298, 0.006299
    );
    var acc = vec3<f32>(0.0);
    for (var k: i32 = 0; k < 13; k = k + 1) {
        acc = acc + textureSample(tex0, samp, i.uv + blur.step * f32(k - 6)).rgb * w[k];
    }
    return vec4<f32>(acc, 1.0);
}
"#;

const SHADER_COMBINE: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VOut {
    var o: VOut;
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    o.uv = uv;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return o;
}
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var tex0: texture_2d<f32>;
@group(0) @binding(2) var tex1: texture_2d<f32>;
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let scene = textureSample(tex0, samp, i.uv).rgb;
    let glow = textureSample(tex1, samp, i.uv).rgb;
    return vec4<f32>(scene + glow, 1.0);
}
"#;
