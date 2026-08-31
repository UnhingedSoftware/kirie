use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use kirie_scene::material::{Blending, CullMode, DepthMode, Pass};
use kirie_shader::reflect::{Parameter, Reflection, SamplerSlot};
use kirie_shader::{IncludeResolver, ShaderInputs, Stage, TranslateError, translate};

use super::blend;
use super::uniforms::{GlType, GlobalsLayout, Member, builtin_type};

#[derive(Debug, thiserror::Error)]
pub enum PassBuildError {
    #[error(transparent)]
    Translate(#[from] TranslateError),
    #[error("VS/FS interface mismatch: fragment reads location {0} the vertex stage does not write")]
    InterfaceMismatch(u32),
    #[error("inter-stage location {0} exceeds the device limit ({1})")]
    TooManyVaryings(u32, u32),
}

pub struct BuiltPass {
    pub pipeline: wgpu::RenderPipeline,
    pub g0_layout: wgpu::BindGroupLayout,
    pub g1_layout: wgpu::BindGroupLayout,
    pub vs_globals: GlobalsLayout,
    pub fs_globals: GlobalsLayout,
    pub vs_samplers: Vec<SamplerSlot>,
    pub fs_samplers: Vec<SamplerSlot>,
    pub g0_bindings: Vec<ModuleBinding>,
    pub g1_bindings: Vec<ModuleBinding>,
    pub vs_params: Vec<Parameter>,
    pub fs_params: Vec<Parameter>,
    pub pos_location: u32,
    pub uv_location: Option<u32>,
}

pub const VERTEX_STRIDE: u64 = 20;

#[allow(clippy::too_many_arguments)]
pub fn build_pass(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    blending: Blending,
    cull: CullMode,
    depthtest: DepthMode,
    depthwrite: DepthMode,
    topology: wgpu::PrimitiveTopology,
    pass: &Pass,
    vs_src: &str,
    fs_src: &str,
    resolver: &dyn IncludeResolver,
) -> Result<BuiltPass, PassBuildError> {
    let base_inputs = shader_inputs(pass);
    let vs_src = sanitize_glsl(vs_src, Stage::Vertex);
    let fs_src = sanitize_glsl(fs_src, Stage::Fragment);

    let mut vs = translate(Stage::Vertex, "pass.vert", &vs_src, resolver, &base_inputs)?;
    let mut fs = translate(Stage::Fragment, "pass.frag", &fs_src, resolver, &base_inputs)?;

    if stages_disagree(&vs.module, &fs.module) {
        let fs_src_stripped = strip_dead_fs_varyings(&vs_src, &fs_src);
        let (vs_src, fs_src) = if has_array_varying(&vs_src) || has_array_varying(&fs_src_stripped) {
            (vs_src.clone(), fs_src_stripped)
        } else {
            pin_varying_locations(&vs_src, &fs_src_stripped)
        };
        let vs0 = translate(Stage::Vertex, "pass.vert", &vs_src, resolver, &base_inputs)?;
        let fs0 = translate(Stage::Fragment, "pass.frag", &fs_src, resolver, &base_inputs)?;
        let mut merged: BTreeMap<String, i32> = vs0.reflection.active_combos.clone();
        for (name, value) in &fs0.reflection.active_combos {
            let slot = merged.entry(name.clone()).or_insert(*value);
            if *value != 0 {
                *slot = *value;
            }
        }
        let inputs = ShaderInputs {
            combos: base_inputs.combos.clone(),
            override_combos: merged,
            populated_texture_slots: base_inputs.populated_texture_slots.clone(),
        };
        vs = translate(Stage::Vertex, "pass.vert", &vs_src, resolver, &inputs)?;
        fs = translate(Stage::Fragment, "pass.frag", &fs_src, resolver, &inputs)?;
    }

    let mut pos_location = 0u32;
    let mut uv_location = None;
    for attr in &vs.reflection.attributes {
        match attr.name.as_str() {
            "a_Position" => pos_location = attr.location,
            "a_TexCoord" => uv_location = Some(attr.location),
            other => {
                return Err(TranslateError::NoMain {
                    file: format!("unsupported vertex attribute {other}"),
                }
                .into());
            }
        }
    }

    let vs_outputs = io_shapes(&vs.module, IoDir::Output);
    let fs_inputs = io_shapes(&fs.module, IoDir::Input);
    for (loc, width) in &fs_inputs {
        if vs_outputs.get(loc).is_none_or(|had| had != width) {
            return Err(PassBuildError::InterfaceMismatch(*loc));
        }
    }

    let max_varyings = device.limits().max_inter_stage_shader_variables;
    if let Some(&loc) = vs_outputs.keys().chain(fs_inputs.keys()).max()
        && loc >= max_varyings
    {
        return Err(PassBuildError::TooManyVaryings(loc, max_varyings));
    }

    let mut fs_module = fs.module;
    for (_h, gv) in fs_module.global_variables.iter_mut() {
        if let Some(binding) = &mut gv.binding {
            binding.group = 1;
        }
    }

    let vs_globals = globals_layout(
        &vs.module,
        &vs.reflection.globals_block,
        &param_types(&vs.reflection),
    );
    let fs_globals = globals_layout(
        &fs_module,
        &fs.reflection.globals_block,
        &param_types(&fs.reflection),
    );

    let g0_bindings = module_bindings(&vs.module);
    let g1_bindings = module_bindings(&fs_module);

    let vs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-scene-vs"),
        source: wgpu::ShaderSource::Naga(Cow::Owned(vs.module)),
    });
    let fs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-scene-fs"),
        source: wgpu::ShaderSource::Naga(Cow::Owned(fs_module)),
    });

    let g0_layout = stage_layout(device, "kirie-scene-g0", wgpu::ShaderStages::VERTEX, &g0_bindings);
    let g1_layout = stage_layout(
        device,
        "kirie-scene-g1",
        wgpu::ShaderStages::FRAGMENT,
        &g1_bindings,
    );

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kirie-scene-pipeline-layout"),
        bind_group_layouts: &[Some(&g0_layout), Some(&g1_layout)],
        immediate_size: 0,
    });

    let mut attrs: Vec<wgpu::VertexAttribute> = vec![wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: pos_location,
    }];
    if let Some(uv) = uv_location {
        attrs.push(wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 12,
            shader_location: uv,
        });
    }
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: VERTEX_STRIDE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-scene-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout)],
        },
        primitive: wgpu::PrimitiveState {
            topology,
            cull_mode: blend::cull_mode(cull),
            front_face: wgpu::FrontFace::Ccw,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: blend::depth_stencil_state(depthtest, depthwrite, wgpu::TextureFormat::Depth24Plus),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &fs_module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(blend::blend_state(blending)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: kirie_platform::pipeline_cache(),
    });

    Ok(BuiltPass {
        pipeline,
        g0_layout,
        g1_layout,
        vs_globals,
        fs_globals,
        vs_samplers: vs.reflection.samplers,
        fs_samplers: fs.reflection.samplers,
        g0_bindings,
        g1_bindings,
        vs_params: vs.reflection.parameters,
        fs_params: fs.reflection.parameters,
        pos_location,
        uv_location,
    })
}

const MODEL_VERTEX_STRIDE: u64 = 48;

#[allow(clippy::too_many_arguments)]
pub fn build_model_pass(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    pass: &Pass,
    vs_src: &str,
    fs_src: &str,
    resolver: &dyn IncludeResolver,
) -> Result<BuiltPass, PassBuildError> {
    let base_inputs = shader_inputs(pass);

    let (vs_src, fs_src) = pin_varying_locations(vs_src, fs_src);

    let vs0 = translate(Stage::Vertex, "model.vert", &vs_src, resolver, &base_inputs)?;
    let fs0 = translate(Stage::Fragment, "model.frag", &fs_src, resolver, &base_inputs)?;
    let mut merged: BTreeMap<String, i32> = vs0.reflection.active_combos.clone();
    for (name, value) in &fs0.reflection.active_combos {
        let slot = merged.entry(name.clone()).or_insert(*value);
        if *value != 0 {
            *slot = *value;
        }
    }
    let inputs = ShaderInputs {
        combos: base_inputs.combos,
        override_combos: merged,
        populated_texture_slots: base_inputs.populated_texture_slots,
    };
    let vs = translate(Stage::Vertex, "model.vert", &vs_src, resolver, &inputs)?;
    let fs = translate(Stage::Fragment, "model.frag", &fs_src, resolver, &inputs)?;

    let mut pos_location = 0u32;
    let mut uv_location = None;
    let mut attrs: Vec<wgpu::VertexAttribute> = Vec::new();
    for attr in &vs.reflection.attributes {
        let (format, offset) = match attr.name.as_str() {
            "a_Position" => {
                pos_location = attr.location;
                (wgpu::VertexFormat::Float32x3, 0)
            }
            "a_Normal" => (wgpu::VertexFormat::Float32x3, 12),
            "a_Tangent4" => (wgpu::VertexFormat::Float32x4, 24),
            "a_TexCoord" => {
                uv_location = Some(attr.location);
                (wgpu::VertexFormat::Float32x2, 40)
            }
            other => {
                return Err(TranslateError::NoMain {
                    file: format!("unsupported model vertex attribute {other}"),
                }
                .into());
            }
        };
        attrs.push(wgpu::VertexAttribute {
            format,
            offset,
            shader_location: attr.location,
        });
    }
    attrs.sort_by_key(|a| a.shader_location);

    let vs_outputs = io_shapes(&vs.module, IoDir::Output);
    let fs_inputs = io_shapes(&fs.module, IoDir::Input);
    for (loc, width) in &fs_inputs {
        if vs_outputs.get(loc).is_none_or(|had| had != width) {
            return Err(PassBuildError::InterfaceMismatch(*loc));
        }
    }
    let max_varyings = device.limits().max_inter_stage_shader_variables;
    if let Some(&loc) = vs_outputs.keys().chain(fs_inputs.keys()).max()
        && loc >= max_varyings
    {
        return Err(PassBuildError::TooManyVaryings(loc, max_varyings));
    }

    let mut fs_module = fs.module;
    for (_h, gv) in fs_module.global_variables.iter_mut() {
        if let Some(binding) = &mut gv.binding {
            binding.group = 1;
        }
    }

    let vs_globals = globals_layout(
        &vs.module,
        &vs.reflection.globals_block,
        &param_types(&vs.reflection),
    );
    let fs_globals = globals_layout(
        &fs_module,
        &fs.reflection.globals_block,
        &param_types(&fs.reflection),
    );
    let g0_bindings = module_bindings(&vs.module);
    let g1_bindings = module_bindings(&fs_module);

    let vs_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-model-vs"),
        source: wgpu::ShaderSource::Naga(Cow::Owned(vs.module)),
    });
    let fs_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-model-fs"),
        source: wgpu::ShaderSource::Naga(Cow::Owned(fs_module)),
    });

    let g0_layout = stage_layout(device, "kirie-model-g0", wgpu::ShaderStages::VERTEX, &g0_bindings);
    let g1_layout = stage_layout(
        device,
        "kirie-model-g1",
        wgpu::ShaderStages::FRAGMENT,
        &g1_bindings,
    );
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kirie-model-pipeline-layout"),
        bind_group_layouts: &[Some(&g0_layout), Some(&g1_layout)],
        immediate_size: 0,
    });

    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: MODEL_VERTEX_STRIDE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-model-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout)],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: blend::cull_mode(pass.cullmode),
            front_face: wgpu::FrontFace::Ccw,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: blend::depth_stencil_state(pass.depthtest, pass.depthwrite, depth_format),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &fs_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(blend::blend_state(pass.blending)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: kirie_platform::pipeline_cache(),
    });

    Ok(BuiltPass {
        pipeline,
        g0_layout,
        g1_layout,
        vs_globals,
        fs_globals,
        vs_samplers: vs.reflection.samplers,
        fs_samplers: fs.reflection.samplers,
        g0_bindings,
        g1_bindings,
        vs_params: vs.reflection.parameters,
        fs_params: fs.reflection.parameters,
        pos_location,
        uv_location,
    })
}

fn pin_varying_locations(vs_src: &str, fs_src: &str) -> (String, String) {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for src in [vs_src, fs_src] {
        for line in src.lines() {
            if let Some(name) = varying_decl_name(line) {
                names.insert(name.to_string());
            }
        }
    }
    let locations: BTreeMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let rewrite = |src: &str| -> String {
        let mut out = String::with_capacity(src.len() + names.len() * 24);
        for (i, line) in src.lines().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match varying_decl_name(line).and_then(|n| locations.get(n).copied()) {
                Some(loc) => {
                    let indent = line.len() - line.trim_start().len();
                    out.push_str(&line[..indent]);
                    out.push_str(&format!("layout(location = {loc}) "));
                    out.push_str(line.trim_start());
                }
                None => out.push_str(line),
            }
        }
        out
    };
    (rewrite(vs_src), rewrite(fs_src))
}

fn strip_dead_fs_varyings(vs_src: &str, fs_src: &str) -> String {
    let vs_names: BTreeSet<&str> = vs_src.lines().filter_map(varying_decl_name).collect();
    let dead: Vec<&str> = fs_src
        .lines()
        .filter_map(varying_decl_name)
        .filter(|n| !vs_names.contains(n))
        .filter(|n| fs_src.matches(n).count() == 1)
        .collect();
    if dead.is_empty() {
        return fs_src.to_string();
    }
    fs_src
        .lines()
        .filter(|line| !varying_decl_name(line).is_some_and(|n| dead.contains(&n)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_array_varying(src: &str) -> bool {
    src.lines().any(|line| {
        line.trim_start()
            .strip_prefix("varying ")
            .is_some_and(|rest| rest.contains('['))
    })
}

fn varying_decl_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("varying ")?;
    if rest.contains('[') {
        return None;
    }
    let name = rest.split_whitespace().nth(1)?.trim_end_matches(';');
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindKind {
    Ubo,
    Texture,
    Sampler,
}

#[derive(Clone, Copy, Debug)]
pub struct ModuleBinding {
    pub binding: u32,
    pub kind: BindKind,
}

fn module_bindings(module: &naga::Module) -> Vec<ModuleBinding> {
    let mut out = Vec::new();
    for (_h, gv) in module.global_variables.iter() {
        let Some(rb) = &gv.binding else { continue };
        let kind = match gv.space {
            naga::AddressSpace::Uniform => BindKind::Ubo,
            naga::AddressSpace::Handle => match module.types[gv.ty].inner {
                naga::TypeInner::Image { .. } => BindKind::Texture,
                naga::TypeInner::Sampler { .. } => BindKind::Sampler,
                _ => continue,
            },
            _ => continue,
        };
        out.push(ModuleBinding {
            binding: rb.binding,
            kind,
        });
    }
    out.sort_by_key(|b| b.binding);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IoDir {
    Output,
    Input,
}

fn io_shapes(module: &naga::Module, dir: IoDir) -> BTreeMap<u32, u8> {
    let mut shapes = BTreeMap::new();
    let Some(ep) = module.entry_points.first() else {
        return shapes;
    };
    let width = |ty: naga::Handle<naga::Type>| match &module.types[ty].inner {
        naga::TypeInner::Scalar(_) => 1_u8,
        naga::TypeInner::Vector { size, .. } => *size as u8,
        _ => 0,
    };
    let mut collect = |binding: Option<&naga::Binding>, ty: naga::Handle<naga::Type>| match binding {
        Some(naga::Binding::Location { location, .. }) => {
            shapes.insert(*location, width(ty));
        }
        _ => {
            if let naga::TypeInner::Struct { members, .. } = &module.types[ty].inner {
                for m in members {
                    if let Some(naga::Binding::Location { location, .. }) = &m.binding {
                        shapes.insert(*location, width(m.ty));
                    }
                }
            }
        }
    };
    match dir {
        IoDir::Output => {
            if let Some(res) = &ep.function.result {
                collect(res.binding.as_ref(), res.ty);
            }
        }
        IoDir::Input => {
            for arg in &ep.function.arguments {
                collect(arg.binding.as_ref(), arg.ty);
            }
        }
    }
    shapes
}

fn stages_disagree(vs: &naga::Module, fs: &naga::Module) -> bool {
    let out = io_shapes(vs, IoDir::Output);
    io_shapes(fs, IoDir::Input)
        .iter()
        .any(|(location, width)| out.get(location).is_none_or(|had| had != width))
}

fn sanitize_glsl(src: &str, stage: Stage) -> String {
    let mut out = String::with_capacity(src.len());
    for (i, line) in src.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("const ")
            && is_const_return_function(rest)
        {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str(rest);
        } else {
            out.push_str(line);
        }
    }
    if stage == Stage::Fragment && out.contains("gl_Position") {
        out = out.replace("gl_Position", "gl_FragCoord");
    }
    out
}

fn is_const_return_function(rest: &str) -> bool {
    for c in rest.chars() {
        match c {
            '(' => return true,
            '=' | ';' => return false,
            _ => {}
        }
    }
    false
}

fn shader_inputs(pass: &Pass) -> ShaderInputs {
    let mut combos = BTreeMap::new();
    for (k, v) in &pass.combos {
        combos.insert(k.clone(), *v as i32);
    }
    let mut slots = std::collections::BTreeSet::new();
    slots.insert(0u32);
    for (i, slot) in pass.textures.iter().enumerate() {
        if slot.is_some() {
            slots.insert(i as u32);
        }
    }
    ShaderInputs {
        combos,
        override_combos: BTreeMap::new(),
        populated_texture_slots: slots,
    }
}

fn globals_layout(
    module: &naga::Module,
    globals_block: &[String],
    param_types: &BTreeMap<String, GlType>,
) -> GlobalsLayout {
    let uniform_ty = module
        .global_variables
        .iter()
        .find_map(|(_h, gv)| (gv.space == naga::AddressSpace::Uniform).then_some(gv.ty));
    let Some(ty) = uniform_ty else {
        return GlobalsLayout::build(globals_block, param_types);
    };
    let naga::TypeInner::Struct { members, span } = &module.types[ty].inner else {
        return GlobalsLayout::build(globals_block, param_types);
    };
    let members = members
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let name = m
                .name
                .clone()
                .or_else(|| globals_block.get(i).cloned())
                .unwrap_or_default();
            let ty = builtin_type(&name)
                .or_else(|| param_types.get(&name).copied())
                .unwrap_or(GlType::Float);
            Member {
                name,
                ty,
                offset: m.offset as usize,
            }
        })
        .collect();
    GlobalsLayout {
        members,
        size: *span as usize,
    }
}

fn param_types(reflection: &Reflection) -> BTreeMap<String, GlType> {
    reflection
        .parameters
        .iter()
        .map(|p| (p.name.clone(), GlType::from_param(p.ty)))
        .collect()
}

fn stage_layout(
    device: &wgpu::Device,
    label: &str,
    visibility: wgpu::ShaderStages,
    bindings: &[ModuleBinding],
) -> wgpu::BindGroupLayout {
    let entries: Vec<wgpu::BindGroupLayoutEntry> = bindings
        .iter()
        .map(|b| wgpu::BindGroupLayoutEntry {
            binding: b.binding,
            visibility,
            ty: match b.kind {
                BindKind::Ubo => wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                BindKind::Texture => wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                BindKind::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            },
            count: None,
        })
        .collect();
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(stage: naga::ShaderStage, src: &str) -> naga::Module {
        let mut front = naga::front::glsl::Frontend::default();
        front
            .parse(&naga::front::glsl::Options::from(stage), src)
            .expect("the test shader parses")
    }

    const VERTEX_WITH_VEC2: &str = "#version 450\nlayout(location = 0) out vec2 v_TexCoord;\nvoid main() { v_TexCoord = vec2(0.0); gl_Position = vec4(0.0); }\n";

    #[test]
    fn a_narrower_varying_than_the_fragment_wants_is_a_disagreement() {
        let vs = parse(naga::ShaderStage::Vertex, VERTEX_WITH_VEC2);
        let fs = parse(
            naga::ShaderStage::Fragment,
            "#version 450\nlayout(location = 0) in vec4 v_TexCoord;\nlayout(location = 0) out vec4 o;\nvoid main() { o = v_TexCoord; }\n",
        );
        assert!(stages_disagree(&vs, &fs));
    }

    #[test]
    fn matching_varyings_agree() {
        let vs = parse(naga::ShaderStage::Vertex, VERTEX_WITH_VEC2);
        let fs = parse(
            naga::ShaderStage::Fragment,
            "#version 450\nlayout(location = 0) in vec2 v_TexCoord;\nlayout(location = 0) out vec4 o;\nvoid main() { o = vec4(v_TexCoord, 0.0, 1.0); }\n",
        );
        assert!(!stages_disagree(&vs, &fs));
    }

    #[test]
    fn a_varying_the_vertex_never_writes_is_a_disagreement() {
        let vs = parse(naga::ShaderStage::Vertex, VERTEX_WITH_VEC2);
        let fs = parse(
            naga::ShaderStage::Fragment,
            "#version 450\nlayout(location = 3) in vec2 v_Other;\nlayout(location = 0) out vec4 o;\nvoid main() { o = vec4(v_Other, 0.0, 1.0); }\n",
        );
        assert!(stages_disagree(&vs, &fs));
    }
}
