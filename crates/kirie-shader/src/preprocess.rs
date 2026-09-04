use std::collections::BTreeMap;

use crate::annotation::{self, UniformAnnotation};
use crate::reflect::{Parameter, Reflection, SamplerSlot, VertexAttribute};
use crate::{IncludeResolver, ShaderInputs, Stage, TranslateError};

pub const PRELUDE_MACROS: &str = r#"precision highp float;
precision highp int;
#define mul(x, y) ((y) * (x))
#define max(x, y) max (y, x)
#define lerp mix
#define frac fract
#define CAST2(x) (vec2(x))
#define CAST3(x) (vec3(x))
#define CAST4(x) (vec4(x))
#define CAST3X3(x) (mat3(x))
#define CASTF(x) (float(x))
#define CASTU(x) (uint(x))
#define float2 vec2
#define float3 vec3
#define float4 vec4
#define int2 ivec2
#define int3 ivec3
#define int4 ivec4
#define saturate(x) (clamp(x, 0.0, 1.0))
#define texSample2D texture
#define texSample2DLod textureLod
#define log10(x) (log2(x) * 0.301029995663981)
#define atan2 atan
#define fmod(x, y) ((x)-(y)*trunc((x)/(y)))
#define ddx dFdx
#define ddy(x) dFdy(-(x))
#define GLSL 1
"#;

const COMPOSE_POSITION: &str = "vec3 position = vec3(a_TexCoord, 0.0);";

fn screen_space_to_texture_space(source: &str) -> String {
    if !source.contains(".xyw;") && !source.contains(COMPOSE_POSITION) {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len() + 64);
    for line in source.split_inclusive('\n') {
        let statement = line.trim();
        let flip = if statement == COMPOSE_POSITION {
            Some("position.y = 1.0 - position.y;".to_owned())
        } else {
            screen_space_target(statement).map(|target| format!("{target}.y = -{target}.y;"))
        };
        let Some(flip) = flip else {
            out.push_str(line);
            continue;
        };
        let body = line.trim_end_matches(['\r', '\n']);
        out.push_str(body);
        out.push(' ');
        out.push_str(&flip);
        out.push_str(&line[body.len()..]);
    }
    out
}

fn screen_space_target(statement: &str) -> Option<&str> {
    let body = statement.strip_suffix(".xyw;")?;
    let (target, source) = body.split_once(" = ")?;
    let is_ident = !target.is_empty() && target.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let is_clip_space = source == "gl_Position"
        || source == "clipSpacePosition"
        || (source.starts_with("mul(") && source.ends_with("ModelViewProjectionMatrix)"));
    (is_ident && is_clip_space).then_some(target)
}

const LIGHTING_V1_STUB: &str = "vec3 PerformLighting_V1(vec3 worldPos, vec3 albedo, vec3 normal, vec3 viewDir, vec3 specularTint, vec3 baseReflectance, float roughness, float metallic) { return vec3(0.0); }\n";

#[derive(Debug, Clone)]
pub struct Assembled {
    pub source: String,
    pub reflection: Reflection,
}

fn header_name(raw: &str) -> String {
    match raw.rsplit_once('.') {
        Some((base, _ext)) => format!("{base}.h"),
        None => format!("{raw}.h"),
    }
}

fn resolve_includes(src: &str, resolver: &dyn IncludeResolver, depth: usize) -> String {
    let mut taken = std::collections::BTreeSet::new();
    include_once(src, resolver, depth, &mut taken)
}

fn include_once(
    src: &str,
    resolver: &dyn IncludeResolver,
    depth: usize,
    taken: &mut std::collections::BTreeSet<String>,
) -> String {
    const MAX_DEPTH: usize = 32;
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#include")
            && let Some(name) = extract_quoted(rest)
        {
            let header = header_name(&name);
            if taken.contains(&header) {
                out.push_str(&format!("// {header} was already included\n"));
                continue;
            }
            if let Some(content) = (depth < MAX_DEPTH).then(|| resolver.resolve(&header)).flatten() {
                taken.insert(header.clone());
                out.push_str(&format!("// begin of include from file {header}\n"));
                out.push_str(&include_once(&content, resolver, depth + 1, taken));
                out.push_str(&format!("\n// end of included from file {header}\n"));
                continue;
            }
            out.push_str(&format!("// tried including file {name} but was not found\n"));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn resolve_requires(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#require") {
            let module = rest.trim();
            if module == "LightingV1" {
                out.push_str(LIGHTING_V1_STUB);
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn extract_quoted(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let end = s[start..].find('"')? + start;
    Some(s[start..end].to_string())
}

fn upper(name: &str) -> String {
    name.to_uppercase()
}

fn sampler_slot(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("g_Texture")?;
    let c = rest.as_bytes().first().copied()?;
    if c.is_ascii_digit() {
        Some((c - b'0') as u32)
    } else {
        None
    }
}

pub fn preprocess(
    stage: Stage,
    filename: &str,
    source: &str,
    resolver: &dyn IncludeResolver,
    inputs: &ShaderInputs,
) -> Result<Assembled, TranslateError> {
    let included = resolve_includes(source, resolver, 0);
    let expanded = resolve_requires(&included);

    if !contains_main(&expanded) {
        return Err(TranslateError::NoMain {
            file: filename.to_string(),
        });
    }

    let mut discovered: BTreeMap<String, i32> = BTreeMap::new();
    let mut combo_requires: BTreeMap<String, BTreeMap<String, i32>> = BTreeMap::new();
    let mut parameters: Vec<Parameter> = Vec::new();
    let mut samplers: Vec<SamplerSlot> = Vec::new();
    let mut attributes: Vec<VertexAttribute> = Vec::new();
    let mut next_attr_loc = 0u32;

    for line in expanded.lines() {
        match annotation::parse_combo_line(line) {
            Ok(Some(combo)) => {
                let name = upper(&combo.combo);
                discovered.entry(name.clone()).or_insert(combo.default);
                if !combo.require.is_empty() {
                    let reqs = combo.require.iter().map(|(k, v)| (upper(k), *v)).collect();
                    combo_requires.insert(name, reqs);
                }
            }
            Ok(None) => {}
            Err(source) => {
                return Err(TranslateError::Annotation {
                    file: filename.to_string(),
                    source,
                });
            }
        }

        if let Ok(Some(uni)) = annotation::parse_uniform_line(line) {
            match uni {
                UniformAnnotation::Parameter {
                    name,
                    ty,
                    material,
                    default,
                } => {
                    if let Some(material) = material {
                        parameters.push(Parameter {
                            name,
                            material,
                            ty,
                            default,
                        });
                    }
                }
                UniformAnnotation::Sampler {
                    name,
                    default_texture,
                    combo,
                    ..
                } => {
                    let slot = sampler_slot(&name);
                    if let Some(ref combo_name) = combo {
                        let populated = slot
                            .map(|s| inputs.populated_texture_slots.contains(&s))
                            .unwrap_or(false)
                            || default_texture.is_some();
                        if populated {
                            discovered.entry(upper(combo_name)).or_insert(1);
                        }
                    }
                    samplers.push(SamplerSlot {
                        name,
                        slot,
                        texture_binding: 0,
                        sampler_binding: 0,
                        default_texture,
                        combo,
                    });
                }
            }
        }
    }

    let active = resolve_combos(&discovered, &combo_requires, inputs);

    let mut active_defs = crate::modernize::collect_defines(&expanded);
    active_defs.extend(active.iter().map(|(k, v)| (k.clone(), Some(i64::from(*v)))));
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cond_stack: Vec<crate::modernize::Tri> = Vec::new();
    for line in expanded.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            crate::modernize::update_cond_stack(&mut cond_stack, trimmed, &active_defs);
            continue;
        }
        if cond_stack.contains(&crate::modernize::Tri::False) {
            continue;
        }
        if parse_attribute(line).is_some() || parse_varying(line).is_some() {
            continue;
        }
        for tok in line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if !tok.is_empty() {
                used.insert(tok.to_string());
            }
        }
    }
    let mut unused_io: Vec<String> = Vec::new();
    let mut varyings: Vec<(String, bool)> = Vec::new();
    cond_stack.clear();
    for line in expanded.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            crate::modernize::update_cond_stack(&mut cond_stack, trimmed, &active_defs);
            continue;
        }
        if cond_stack.contains(&crate::modernize::Tri::False) {
            continue;
        }
        if let Some(attr) = parse_attribute(line) {
            if stage == Stage::Vertex {
                if used.contains(&attr) {
                    attributes.push(VertexAttribute {
                        name: attr,
                        location: next_attr_loc,
                    });
                    next_attr_loc += 1;
                } else {
                    unused_io.push(attr);
                }
            }
        } else if let Some(v) = parse_varying(line) {
            let is_used = used.contains(&v);
            varyings.push((v, is_used));
        }
    }
    for (name, is_used) in varyings.iter().rev() {
        if *is_used {
            break;
        }
        unused_io.push(name.clone());
    }

    let expanded = booleanize_combo_conditions(&expanded, &active);
    let expanded = screen_space_to_texture_space(&expanded);
    let body = expanded.replace("gl_FragColor", "out_FragColor");
    let body = if unused_io.is_empty() {
        body
    } else {
        body.lines()
            .filter(
                |line| match parse_attribute(line).or_else(|| parse_varying(line)) {
                    Some(name) => !unused_io.contains(&name),
                    None => true,
                },
            )
            .collect::<Vec<_>>()
            .join("\n")
    };

    let synth = synth_missing_engine_uniforms(&body);
    let body = if synth.is_empty() {
        body
    } else {
        format!("{synth}{body}")
    };

    let mut source_out = String::new();
    source_out.push_str(&filtered_prelude(&body));
    source_out.push_str(stage_defines(stage));
    for (name, value) in &active {
        source_out.push_str(&format!("#define {name} {value}\n"));
    }
    source_out.push('\n');
    source_out.push_str(&body);

    Ok(Assembled {
        source: source_out,
        reflection: Reflection {
            globals_block: Vec::new(),
            parameters,
            samplers,
            attributes,
            active_combos: active,
        },
    })
}

fn synth_missing_engine_uniforms(body: &str) -> String {
    use std::collections::{BTreeMap, BTreeSet};

    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut widths: BTreeMap<String, u8> = BTreeMap::new();

    for raw in body.lines() {
        let line = raw.split("//").next().unwrap_or("");
        let trimmed = line.trim_start();
        let is_decl = trimmed.starts_with("uniform ")
            || trimmed.starts_with("attribute ")
            || trimmed.starts_with("varying ");
        if is_decl {
            if let Some(name) = decl_name(line) {
                declared.insert(name);
            }
            continue;
        }
        scan_engine_uses(line, &mut widths);
    }

    let mut out = String::new();
    for (name, width) in &widths {
        if declared.contains(name) {
            continue;
        }
        let ty = match width {
            2 => "vec2",
            3 => "vec3",
            4 => "vec4",
            _ => "float",
        };
        out.push_str(&format!("uniform {ty} {name};\n"));
    }
    out
}

fn decl_name(line: &str) -> Option<String> {
    let code = line.split("//").next().unwrap_or("").trim();
    let code = code.strip_suffix(';').unwrap_or(code);
    let head = code.split(['[', '=']).next().unwrap_or(code).trim();
    let name = head.split_whitespace().next_back()?;
    name.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        .then(|| name.to_string())
}

fn scan_engine_uses(line: &str, widths: &mut std::collections::BTreeMap<String, u8>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let name = &line[start..i];
            if name.starts_with("g_") {
                let mut width = 1u8;
                if bytes.get(i) == Some(&b'.') {
                    let sw_start = i + 1;
                    let mut j = sw_start;
                    while j < bytes.len() && is_ident_byte(bytes[j]) {
                        j += 1;
                    }
                    let sw = &line[sw_start..j];
                    if !sw.is_empty() && sw.bytes().all(|c| b"xyzwrgbastpq".contains(&c)) {
                        width = sw.bytes().map(swizzle_component).max().unwrap_or(0) + 1;
                    }
                }
                let e = widths.entry(name.to_string()).or_insert(1);
                *e = (*e).max(width);
            }
            continue;
        }
        i += 1;
    }
}

fn swizzle_component(c: u8) -> u8 {
    match c {
        b'x' | b'r' | b's' => 0,
        b'y' | b'g' | b't' => 1,
        b'z' | b'b' | b'p' => 2,
        _ => 3,
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

const TYPE_KEYWORDS: &[&str] = &[
    "void", "float", "int", "uint", "bool", "vec2", "vec3", "vec4", "ivec2", "ivec3", "ivec4", "uvec2",
    "uvec3", "uvec4", "bvec2", "bvec3", "bvec4", "mat2", "mat3", "mat4",
];

fn filtered_prelude(body: &str) -> String {
    let mut out = String::with_capacity(PRELUDE_MACROS.len());
    for line in PRELUDE_MACROS.lines() {
        if let Some(name) = function_macro_name(line)
            && body_defines_function(body, name)
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn function_macro_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("#define ")?;
    let paren = rest.find('(')?;
    let name = &rest[..paren];
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
}

fn body_defines_function(body: &str, name: &str) -> bool {
    let bytes = body.as_bytes();
    let mut search = 0;
    while let Some(rel) = body[search..].find(name) {
        let start = search + rel;
        let end = start + name.len();
        search = end;
        let before_ok = start
            .checked_sub(1)
            .map(|b| !(bytes[b].is_ascii_alphanumeric() || bytes[b] == b'_'))
            .unwrap_or(true);
        if !before_ok || !body[end..].trim_start().starts_with('(') {
            continue;
        }
        let prefix = body[..start].trim_end();
        let prev_tok = prefix
            .rsplit(|c: char| c.is_whitespace() || c == ';' || c == '}')
            .next();
        if prev_tok.is_some_and(|t| TYPE_KEYWORDS.contains(&t)) {
            return true;
        }
    }
    false
}

fn stage_defines(stage: Stage) -> &'static str {
    match stage {
        Stage::Fragment => "out vec4 out_FragColor;\n#define varying in\n",
        Stage::Vertex => "#define attribute in\n#define varying out\n",
    }
}

fn contains_main(src: &str) -> bool {
    let bytes = src.as_bytes();
    let mut i = 0;
    while let Some(pos) = src[i..].find("main") {
        let at = i + pos;
        let before = at.checked_sub(1).map(|b| bytes[b]);
        let after = bytes.get(at + 4).copied();
        let word_before = before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != b'_');
        let ok_after = matches!(
            after,
            Some(b'(') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'{')
        );
        if word_before && ok_after {
            return true;
        }
        i = at + 4;
    }
    false
}

fn parse_attribute(line: &str) -> Option<String> {
    let code = line.split("//").next().unwrap_or("").trim();
    let rest = code.strip_prefix("attribute ")?;
    let decl = rest.strip_suffix(';')?;
    let name = decl.split_whitespace().next_back()?;
    Some(name.split('[').next().unwrap_or(name).to_string())
}

fn parse_varying(line: &str) -> Option<String> {
    let code = line.split("//").next().unwrap_or("").trim();
    let rest = code.strip_prefix("varying ")?;
    let decl = rest.strip_suffix(';')?;
    let name = decl.split_whitespace().next_back()?;
    Some(name.split('[').next().unwrap_or(name).to_string())
}

fn resolve_combos(
    discovered: &BTreeMap<String, i32>,
    combo_requires: &BTreeMap<String, BTreeMap<String, i32>>,
    inputs: &ShaderInputs,
) -> BTreeMap<String, i32> {
    let mut values: BTreeMap<String, i32> = discovered.clone();
    for (k, v) in &inputs.combos {
        values.insert(upper(k), *v);
    }
    for (k, v) in &inputs.override_combos {
        values.insert(upper(k), *v);
    }
    for _ in 0..16 {
        let mut changed = false;
        let snapshot = values.clone();
        for (name, value) in &snapshot {
            if *value == 0 {
                continue;
            }
            if let Some(reqs) = combo_requires.get(name) {
                for (req_name, req_val) in reqs {
                    if values.get(req_name) != Some(req_val) {
                        values.insert(req_name.clone(), *req_val);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    values
}

fn booleanize_combo_conditions(source: &str, combos: &BTreeMap<String, i32>) -> String {
    if combos.is_empty() {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            out.push_str(line);
            continue;
        }
        out.push_str(&booleanize_line(line, combos));
    }
    out
}

fn booleanize_line(line: &str, combos: &BTreeMap<String, i32>) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut at = 0usize;
    while at < chars.len() {
        let ch = chars[at];
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = at;
            while at < chars.len() && (chars[at].is_ascii_alphanumeric() || chars[at] == '_') {
                at += 1;
            }
            let word: String = chars[start..at].iter().collect();
            if combos.contains_key(&word) && wants_bool(&chars, start, at) {
                out.push_str("bool(");
                out.push_str(&word);
                out.push(')');
            } else {
                out.push_str(&word);
            }
        } else {
            out.push(ch);
            at += 1;
        }
    }
    out
}

fn wants_bool(chars: &[char], start: usize, end: usize) -> bool {
    let mut after = end;
    while chars.get(after).is_some_and(|c| c.is_whitespace()) {
        after += 1;
    }
    let next = chars.get(after).copied();
    if next == Some('?') {
        return true;
    }
    if matches!(next, Some('&' | '|')) && chars.get(after + 1) == next.as_ref().copied().as_ref() {
        return true;
    }

    let mut before = start;
    while before > 0 && chars[before - 1].is_whitespace() {
        before -= 1;
    }
    if before >= 2 {
        let prev = chars[before - 1];
        if matches!(prev, '&' | '|') && chars[before - 2] == prev {
            return true;
        }
    }
    if next == Some(')') && before > 0 && chars[before - 1] == '(' {
        let mut word_end = before - 1;
        while word_end > 0 && chars[word_end - 1].is_whitespace() {
            word_end -= 1;
        }
        let mut word_start = word_end;
        while word_start > 0
            && (chars[word_start - 1].is_ascii_alphanumeric() || chars[word_start - 1] == '_')
        {
            word_start -= 1;
        }
        let keyword: String = chars[word_start..word_end].iter().collect();
        return keyword == "if" || keyword == "while";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IncludeResolver;
    use std::collections::BTreeMap;

    struct MapResolver(BTreeMap<String, String>);
    impl IncludeResolver for MapResolver {
        fn resolve(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    fn run(stage: Stage, src: &str, headers: &[(&str, &str)]) -> Assembled {
        let map = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        preprocess(stage, "unit", src, &MapResolver(map), &ShaderInputs::default()).unwrap()
    }
    #[test]
    fn a_screen_position_is_flipped_into_texture_space() {
        let out = screen_space_to_texture_space("	v_ScreenPos = gl_Position.xyw;\n#ifdef HLSL\n");
        assert_eq!(
            out,
            "	v_ScreenPos = gl_Position.xyw; v_ScreenPos.y = -v_ScreenPos.y;\n#ifdef HLSL\n"
        );
    }

    #[test]
    fn a_projected_screen_coordinate_is_flipped_into_texture_space() {
        for line in [
            "	v_ScreenCoord = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix).xyw;\n",
            "	v_ScreenCoord = mul(vec4((a_Position), 1.0), g_EffectModelViewProjectionMatrix).xyw;\n",
            "	screenSpacePosition = clipSpacePosition.xyw;\n",
        ] {
            let out = screen_space_to_texture_space(line);
            let target = line.trim().split_once(" = ").unwrap().0;
            assert!(out.contains(&format!("{target}.y = -{target}.y;")), "{out}");
        }
    }

    #[test]
    fn an_unprojected_or_compound_xyw_is_left_alone() {
        for source in [
            "	v_PointerUV.xyz = mul(vec4(pointer * 2 - 1, 0.0, 1.0), g_ModelViewProjectionMatrixInverse).xyw;",
            "	force.xyw += left.xyw;",
            "	vec4 Q = (RGB.r < P.x) ? vec4(P.xyw, RGB.r) : vec4(RGB.r, P.yzx);",
        ] {
            assert_eq!(screen_space_to_texture_space(source), source);
        }
    }

    #[test]
    fn the_compose_layer_position_is_flipped_into_texture_space() {
        let out = screen_space_to_texture_space("	vec3 position = vec3(a_TexCoord, 0.0);\n#if FOO\n");
        assert_eq!(
            out,
            "	vec3 position = vec3(a_TexCoord, 0.0); position.y = 1.0 - position.y;\n#if FOO\n"
        );
    }

    #[test]
    fn a_shader_without_a_screen_position_is_left_alone() {
        let source = "void main() { gl_Position = vec4(0.0); }";
        assert_eq!(screen_space_to_texture_space(source), source);
    }

    #[test]
    fn include_is_inlined_before_main() {
        let a = run(
            Stage::Fragment,
            "#include \"common.h\"\nvoid main() { float x = HELPER; }",
            &[("common.h", "#define HELPER 3.0\n")],
        );
        assert!(a.source.contains("#define HELPER 3.0"));
        assert!(a.source.contains("begin of include from file common.h"));
    }

    #[test]
    fn missing_include_is_not_error() {
        let a = run(Stage::Fragment, "#include \"nope.h\"\nvoid main() {}", &[]);
        assert!(a.source.contains("tried including file nope.h but was not found"));
    }

    #[test]
    fn unused_trailing_varying_and_attribute_are_elided() {
        let vs = run(
            Stage::Vertex,
            "attribute vec3 a_Position;\nattribute vec4 a_Color;\n#ifdef VERTEXCOLOR\nvarying vec4 v_Color;\n#endif\nvoid main() {\n gl_Position = vec4(a_Position, 1.0);\n#ifdef VERTEXCOLOR\n v_Color = a_Color;\n#endif\n}",
            &[],
        );
        let attrs: Vec<&str> = vs.reflection.attributes.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            attrs,
            vec!["a_Position"],
            "unused a_Color dropped from reflection"
        );
        assert!(
            !vs.source.contains("attribute vec4 a_Color"),
            "unused a_Color declaration removed from source"
        );

        let fs = run(
            Stage::Fragment,
            "varying vec4 v_Color;\nvoid main() {\n gl_FragColor = vec4(1.0);\n#ifdef VERTEXCOLOR\n gl_FragColor *= v_Color;\n#endif\n}",
            &[],
        );
        assert!(
            !fs.source.contains("varying vec4 v_Color"),
            "unused trailing v_Color declaration removed"
        );
    }

    #[test]
    fn unused_non_trailing_varying_is_retained() {
        let fs = run(
            Stage::Fragment,
            "varying vec2 v_TexCoord;\nvarying vec3 v_ScreenCoord;\nvoid main() {\n gl_FragColor = vec4(v_ScreenCoord, 1.0);\n}",
            &[],
        );
        assert!(
            fs.source.contains("v_TexCoord"),
            "non-trailing unused varying kept to preserve locations"
        );
        assert!(fs.source.contains("v_ScreenCoord"));
    }

    #[test]
    fn combo_discovered_default_emitted_uppercased() {
        let a = run(
            Stage::Fragment,
            "// [COMBO] {\"combo\":\"lighting\",\"default\":2}\nvoid main() {}",
            &[],
        );
        assert!(a.source.contains("#define LIGHTING 2"));
        assert_eq!(a.reflection.active_combos.get("LIGHTING"), Some(&2));
    }

    #[test]
    fn require_chain_promotes_dependency() {
        let src = "// [COMBO] {\"combo\":\"LIGHTING\",\"default\":0}\n\
                   // [COMBO] {\"combo\":\"RIMLIGHTING\",\"default\":1,\"require\":{\"LIGHTING\":1}}\n\
                   void main() {}";
        let a = run(Stage::Fragment, src, &[]);
        assert_eq!(a.reflection.active_combos.get("LIGHTING"), Some(&1));
    }

    #[test]
    fn gl_fragcolor_rewritten() {
        let a = run(Stage::Fragment, "void main() { gl_FragColor = vec4(1.0); }", &[]);
        assert!(a.source.contains("out_FragColor = vec4(1.0)"));
        assert!(!a.source.contains("gl_FragColor"));
        assert!(a.source.contains("out vec4 out_FragColor;"));
    }

    #[test]
    fn stage_defines_differ() {
        let f = run(Stage::Fragment, "void main() {}", &[]);
        assert!(f.source.contains("#define varying in"));
        let v = run(Stage::Vertex, "void main() {}", &[]);
        assert!(v.source.contains("#define attribute in"));
        assert!(v.source.contains("#define varying out"));
    }

    #[test]
    fn require_module_lightingv1_stub() {
        let a = run(Stage::Fragment, "#require LightingV1\nvoid main() {}", &[]);
        assert!(a.source.contains("PerformLighting_V1"));
    }

    #[test]
    fn no_main_is_error() {
        let map = BTreeMap::new();
        let err = preprocess(
            Stage::Fragment,
            "u",
            "float x = 1.0;",
            &MapResolver(map),
            &ShaderInputs::default(),
        );
        assert!(matches!(err, Err(TranslateError::NoMain { .. })));
    }

    #[test]
    fn sampler_slot_and_param_reflected() {
        let src = "uniform sampler2D g_Texture0; // {\"default\":\"util/white\"}\n\
                   uniform float g_Brightness; // {\"material\":\"Brightness\",\"default\":1}\n\
                   void main() {}";
        let a = run(Stage::Fragment, src, &[]);
        assert_eq!(a.reflection.samplers.len(), 1);
        assert_eq!(a.reflection.samplers[0].slot, Some(0));
        assert_eq!(a.reflection.parameters.len(), 1);
        assert_eq!(a.reflection.parameters[0].material, "Brightness");
    }

    #[test]
    fn material_combo_overrides_discovered_default() {
        let mut inputs = ShaderInputs::default();
        inputs.combos.insert("lighting".into(), 0);
        let map = BTreeMap::new();
        let a = preprocess(
            Stage::Fragment,
            "u",
            "// [COMBO] {\"combo\":\"LIGHTING\",\"default\":1}\nvoid main() {}",
            &MapResolver(map),
            &inputs,
        )
        .unwrap();
        assert_eq!(a.reflection.active_combos.get("LIGHTING"), Some(&0));
    }
}
