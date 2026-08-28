use crate::Stage;
use crate::preprocess::Assembled;
use crate::reflect::{Reflection, SamplerSlot};

const VERSION_LINE: &str = "#version 450\n";

const GLOBALS_BLOCK: &str = "_WEGlobals";

const RESERVED: &[&str] = &[
    "sample",
    "filter",
    "input",
    "output",
    "active",
    "partition",
    "common",
    "superp",
    "resource",
    "patch",
];

pub fn modernize(_stage: Stage, assembled: Assembled) -> (String, Reflection) {
    let Assembled {
        source,
        mut reflection,
    } = assembled;

    let renamed = rename_reserved(&source);

    let defines = collect_defines(&renamed);

    let mut block_members: Vec<String> = Vec::new();
    let mut sampler_decls = String::new();
    let mut body = String::new();
    let mut next_binding = 1u32;
    let mut cond_stack: Vec<Tri> = Vec::new();

    for line in renamed.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            update_cond_stack(&mut cond_stack, trimmed, &defines);
            body.push_str(line);
            body.push('\n');
            continue;
        }
        let inactive = cond_stack.contains(&Tri::False);

        if !inactive && let Some(decl) = parse_uniform_decl(line) {
            match decl {
                UniformDecl::Sampler(name) => {
                    let tex_b = next_binding;
                    let smp_b = next_binding + 1;
                    next_binding += 2;
                    sampler_decls.push_str(&format!(
                        "layout(set = 0, binding = {tex_b}) uniform texture2D {name}_img;\n\
                         layout(set = 0, binding = {smp_b}) uniform sampler {name}_smp;\n\
                         #define {name} sampler2D({name}_img, {name}_smp)\n"
                    ));
                    if let Some(slot) = reflection.samplers.iter_mut().find(|s| s.name == name) {
                        slot.texture_binding = tex_b;
                        slot.sampler_binding = smp_b;
                    } else {
                        reflection.samplers.push(SamplerSlot {
                            slot: sampler_slot(&name),
                            name,
                            texture_binding: tex_b,
                            sampler_binding: smp_b,
                            default_texture: None,
                            combo: None,
                        });
                    }
                    continue;
                }
                UniformDecl::Loose(member) => {
                    block_members.push(resolve_macro_array_sizes(&member, &defines));
                    continue;
                }
                UniformDecl::Other => { /* fall through, keep the line */ }
            }
        }
        body.push_str(line);
        body.push('\n');
    }

    let mut out = String::with_capacity(renamed.len() + 256);
    out.push_str(VERSION_LINE);
    if !block_members.is_empty() {
        out.push_str(&format!(
            "layout(std140, set = 0, binding = 0) uniform {GLOBALS_BLOCK} {{\n"
        ));
        for m in &block_members {
            out.push_str("    ");
            out.push_str(m);
            out.push_str(";\n");
        }
        out.push_str("};\n");
    }
    out.push_str(&sampler_decls);
    out.push_str(&body);

    reflection.globals_block = block_members.iter().map(|m| member_name(m).to_string()).collect();

    (out, reflection)
}

fn resolve_macro_array_sizes(
    member: &str,
    defines: &std::collections::HashMap<String, Option<i64>>,
) -> String {
    let mut out = String::with_capacity(member.len());
    let mut rest = member;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open + 1..].find(']') else {
            break;
        };
        let name = rest[open + 1..open + 1 + close_rel].trim();
        out.push_str(&rest[..=open]);
        match defines.get(name) {
            Some(Some(v)) if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                out.push_str(&v.to_string());
            }
            _ => out.push_str(name),
        }
        out.push(']');
        rest = &rest[open + 2 + close_rel..];
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tri {
    True,
    False,
    Unknown,
}

fn collect_defines(src: &str) -> std::collections::HashMap<String, Option<i64>> {
    let mut out = std::collections::HashMap::new();
    for line in src.lines() {
        let Some(rest) = line.trim_start().strip_prefix("#define ") else {
            continue;
        };
        let rest = rest.trim_start();
        let name_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let name = &rest[..name_end];
        if name.is_empty() || name.contains('(') {
            continue;
        }
        let body = rest[name_end..].split("//").next().unwrap_or("").trim();
        out.insert(name.to_string(), body.parse::<i64>().ok());
    }
    out
}

fn eval_directive(directive: &str, defines: &std::collections::HashMap<String, Option<i64>>) -> Tri {
    let d = directive.split("//").next().unwrap_or("").trim();
    let defined = |name: &str| defines.contains_key(name);
    if let Some(name) = d.strip_prefix("#ifdef ") {
        return tri(defined(name.trim()));
    }
    if let Some(name) = d.strip_prefix("#ifndef ") {
        return tri(!defined(name.trim()));
    }
    let Some(expr) = d.strip_prefix("#if ").or_else(|| d.strip_prefix("#elif ")) else {
        return Tri::Unknown;
    };
    let expr = expr.trim();
    if let Ok(n) = expr.parse::<i64>() {
        return tri(n != 0);
    }
    if let Some(inner) = expr.strip_prefix("defined(").and_then(|s| s.strip_suffix(')')) {
        return tri(defined(inner.trim()));
    }
    if !expr.is_empty() && expr.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return match defines.get(expr) {
            None => Tri::False,
            Some(Some(v)) => tri(*v != 0),
            Some(None) => Tri::Unknown,
        };
    }
    Tri::Unknown
}

fn tri(b: bool) -> Tri {
    if b { Tri::True } else { Tri::False }
}

pub(crate) fn update_cond_stack(
    stack: &mut Vec<Tri>,
    directive: &str,
    defines: &std::collections::HashMap<String, Option<i64>>,
) {
    let d = directive.trim_start();
    if d.starts_with("#ifdef") || d.starts_with("#ifndef") || d.starts_with("#if ") || d == "#if" {
        stack.push(eval_directive(d, defines));
    } else if d.starts_with("#elif") {
        if let Some(top) = stack.last_mut() {
            *top = Tri::Unknown;
        }
    } else if d.starts_with("#else") {
        if let Some(top) = stack.last_mut() {
            *top = match *top {
                Tri::True => Tri::False,
                Tri::False => Tri::True,
                Tri::Unknown => Tri::Unknown,
            };
        }
    } else if d.starts_with("#endif") {
        stack.pop();
    }
}

enum UniformDecl {
    Sampler(String),
    Loose(String),
    Other,
}

fn parse_uniform_decl(line: &str) -> Option<UniformDecl> {
    let code = line.split("//").next().unwrap_or("").trim();
    let rest = code.strip_prefix("uniform ")?;
    let inner = rest.strip_suffix(';')?;
    if inner.contains('{') || inner.contains('=') {
        return Some(UniformDecl::Other);
    }
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    if tokens.len() < 2 {
        return Some(UniformDecl::Other);
    }
    let ty = tokens[0];
    if ty == "sampler2D" {
        let name = tokens[tokens.len() - 1];
        if name.contains('[') {
            return Some(UniformDecl::Other);
        }
        return Some(UniformDecl::Sampler(name.to_string()));
    }
    if ty.starts_with("sampler") || ty.starts_with("texture") || ty.starts_with("image") {
        return Some(UniformDecl::Other);
    }
    Some(UniformDecl::Loose(inner.trim().to_string()))
}

fn sampler_slot(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("g_Texture")?;
    let c = rest.as_bytes().first().copied()?;
    c.is_ascii_digit().then(|| u32::from(c - b'0'))
}

fn member_name(member: &str) -> &str {
    let name = member.split_whitespace().next_back().unwrap_or(member);
    name.split('[').next().unwrap_or(name)
}

fn rename_reserved(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    let mut word = String::new();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for ch in src.chars() {
        if is_word(ch) {
            word.push(ch);
        } else {
            flush_word(&mut word, &mut out);
            out.push(ch);
        }
    }
    flush_word(&mut word, &mut out);
    out
}

fn flush_word(word: &mut String, out: &mut String) {
    if word.is_empty() {
        return;
    }
    if RESERVED.contains(&word.as_str()) {
        out.push_str(word);
        out.push_str("_we");
    } else {
        out.push_str(word);
    }
    word.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocess::Assembled;
    use crate::reflect::{Reflection, SamplerSlot};

    fn asm(source: &str, samplers: Vec<SamplerSlot>) -> Assembled {
        Assembled {
            source: source.to_string(),
            reflection: Reflection {
                samplers,
                ..Reflection::default()
            },
        }
    }

    fn slot(name: &str) -> SamplerSlot {
        SamplerSlot {
            name: name.to_string(),
            slot: Some(0),
            texture_binding: 0,
            sampler_binding: 0,
            default_texture: None,
            combo: None,
        }
    }

    #[test]
    fn version_line_prepended() {
        let (src, _) = modernize(Stage::Fragment, asm("void main() {}", vec![]));
        assert!(src.starts_with("#version 450"));
    }

    #[test]
    fn loose_uniforms_packed_with_semicolons() {
        let (src, refl) = modernize(
            Stage::Fragment,
            asm(
                "uniform float g_Time;\nuniform vec2 g_TexelSize;\nvoid main() {}",
                vec![],
            ),
        );
        assert!(src.contains("uniform _WEGlobals {"));
        assert!(src.contains("    float g_Time;"));
        assert!(src.contains("    vec2 g_TexelSize;"));
        assert_eq!(refl.globals_block, vec!["g_Time", "g_TexelSize"]);
        assert!(!src.contains("\nuniform float g_Time;"));
    }

    #[test]
    fn array_uniform_packed_member_name() {
        let (_, refl) = modernize(
            Stage::Fragment,
            asm("uniform float g_AudioSpectrum16Left[16];\nvoid main() {}", vec![]),
        );
        assert_eq!(refl.globals_block, vec!["g_AudioSpectrum16Left"]);
    }

    #[test]
    fn combined_sampler_split_with_pairing_macro() {
        let (src, refl) = modernize(
            Stage::Fragment,
            asm(
                "uniform sampler2D g_Texture0;\nvoid main() {}",
                vec![slot("g_Texture0")],
            ),
        );
        assert!(src.contains("uniform texture2D g_Texture0_img;"));
        assert!(src.contains("uniform sampler g_Texture0_smp;"));
        assert!(src.contains("#define g_Texture0 sampler2D(g_Texture0_img, g_Texture0_smp)"));
        assert_eq!(refl.samplers[0].texture_binding, 1);
        assert_eq!(refl.samplers[0].sampler_binding, 2);
    }

    #[test]
    fn unannotated_sampler_is_reflected() {
        let (src, refl) = modernize(
            Stage::Fragment,
            asm(
                "uniform sampler2D g_Texture0;\nuniform sampler2D g_Texture1;\nvoid main() {}",
                vec![],
            ),
        );
        assert!(src.contains("uniform texture2D g_Texture0_img;"));
        assert!(src.contains("uniform texture2D g_Texture1_img;"));
        assert_eq!(refl.samplers.len(), 2);
        assert_eq!(refl.samplers[0].name, "g_Texture0");
        assert_eq!(refl.samplers[0].slot, Some(0));
        assert_eq!(refl.samplers[0].texture_binding, 1);
        assert_eq!(refl.samplers[0].sampler_binding, 2);
        assert_eq!(refl.samplers[1].name, "g_Texture1");
        assert_eq!(refl.samplers[1].slot, Some(1));
        assert_eq!(refl.samplers[1].texture_binding, 3);
        assert_eq!(refl.samplers[1].sampler_binding, 4);
    }

    #[test]
    fn reserved_word_renamed() {
        let (src, _) = modernize(
            Stage::Fragment,
            asm("void main() { float sample = 1.0; }", vec![]),
        );
        assert!(src.contains("float sample_we = 1.0;"));
        let (src2, _) = modernize(
            Stage::Fragment,
            asm("void main() { int sampleCount = 0; }", vec![]),
        );
        assert!(src2.contains("sampleCount"));
        assert!(!src2.contains("sampleCount_we"));
    }
}
