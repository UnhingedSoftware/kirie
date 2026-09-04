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

    let renamed = drop_stray_endifs(&rename_reserved(&source));

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

pub(crate) fn collect_defines(src: &str) -> std::collections::HashMap<String, Option<i64>> {
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

fn drop_stray_endifs(source: &str) -> String {
    let mut depth = 0_i32;
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let directive = line.trim_start();
        if directive.starts_with("#if") {
            depth += 1;
        } else if directive.starts_with("#endif") {
            if depth == 0 {
                continue;
            }
            depth -= 1;
        }
        out.push_str(line);
    }
    out
}

fn eval_directive(directive: &str, defines: &std::collections::HashMap<String, Option<i64>>) -> Tri {
    let d = directive.split("//").next().unwrap_or("").trim();
    if let Some(name) = d.strip_prefix("#ifdef ") {
        return tri(defines.contains_key(name.trim()));
    }
    if let Some(name) = d.strip_prefix("#ifndef ") {
        return tri(!defines.contains_key(name.trim()));
    }
    let Some(expr) = d.strip_prefix("#if ").or_else(|| d.strip_prefix("#elif ")) else {
        return Tri::Unknown;
    };
    let tokens = tokenize(expr);
    let mut parser = ExprParser {
        tokens: &tokens,
        pos: 0,
        defines,
    };
    let value = parser.or();
    if parser.pos != tokens.len() {
        return Tri::Unknown;
    }
    value.truth()
}

#[derive(Clone, Copy, PartialEq)]
enum Val {
    Num(i64),
    Unknown,
}

impl Val {
    fn truth(self) -> Tri {
        match self {
            Val::Num(n) => tri(n != 0),
            Val::Unknown => Tri::Unknown,
        }
    }

    fn from_tri(t: Tri) -> Val {
        match t {
            Tri::True => Val::Num(1),
            Tri::False => Val::Num(0),
            Tri::Unknown => Val::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(i64),
    Op(&'static str),
}

fn tokenize(expr: &str) -> Vec<Tok> {
    const OPS: [&str; 12] = ["&&", "||", ">=", "<=", "==", "!=", ">", "<", "!", "(", ")", ","];
    let mut out = Vec::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            match expr[start..i].parse::<i64>() {
                Ok(n) => out.push(Tok::Num(n)),
                Err(_) => out.push(Tok::Ident(expr[start..i].to_owned())),
            }
        } else if is_ident_start(c) {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            out.push(Tok::Ident(expr[start..i].to_owned()));
        } else if let Some(op) = OPS.iter().find(|op| expr[i..].starts_with(**op)) {
            out.push(Tok::Op(op));
            i += op.len();
        } else {
            out.push(Tok::Ident(String::new()));
            i += 1;
        }
    }
    out
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

struct ExprParser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    defines: &'a std::collections::HashMap<String, Option<i64>>,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Val {
        let mut acc = self.and().truth();
        while self.eat("||") {
            let rhs = self.and().truth();
            acc = match (acc, rhs) {
                (Tri::True, _) | (_, Tri::True) => Tri::True,
                (Tri::False, Tri::False) => Tri::False,
                _ => Tri::Unknown,
            };
        }
        Val::from_tri(acc)
    }

    fn and(&mut self) -> Val {
        let mut acc = self.not().truth();
        while self.eat("&&") {
            let rhs = self.not().truth();
            acc = match (acc, rhs) {
                (Tri::False, _) | (_, Tri::False) => Tri::False,
                (Tri::True, Tri::True) => Tri::True,
                _ => Tri::Unknown,
            };
        }
        Val::from_tri(acc)
    }

    fn not(&mut self) -> Val {
        if self.eat("!") {
            return match self.not().truth() {
                Tri::True => Val::Num(0),
                Tri::False => Val::Num(1),
                Tri::Unknown => Val::Unknown,
            };
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Val {
        let lhs = self.prim();
        for op in [">=", "<=", "==", "!=", ">", "<"] {
            if self.eat(op) {
                let rhs = self.prim();
                let (Val::Num(a), Val::Num(b)) = (lhs, rhs) else {
                    return Val::Unknown;
                };
                return Val::from_tri(tri(match op {
                    ">=" => a >= b,
                    "<=" => a <= b,
                    "==" => a == b,
                    "!=" => a != b,
                    ">" => a > b,
                    _ => a < b,
                }));
            }
        }
        lhs
    }

    fn prim(&mut self) -> Val {
        if self.eat("(") {
            let inner = self.or();
            if !self.eat(")") {
                return Val::Unknown;
            }
            return inner;
        }
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Val::Num(n)
            }
            Some(Tok::Ident(name)) if name == "defined" => {
                self.pos += 1;
                let paren = self.eat("(");
                let Some(Tok::Ident(target)) = self.peek().cloned() else {
                    return Val::Unknown;
                };
                self.pos += 1;
                if paren && !self.eat(")") {
                    return Val::Unknown;
                }
                Val::Num(i64::from(self.defines.contains_key(&target)))
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                match self.defines.get(&name) {
                    None => Val::Num(0),
                    Some(Some(v)) => Val::Num(*v),
                    Some(None) => Val::Unknown,
                }
            }
            _ => Val::Unknown,
        }
    }
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

    fn defines(pairs: &[(&str, Option<i64>)]) -> std::collections::HashMap<String, Option<i64>> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), *value))
            .collect()
    }

    #[test]
    fn an_undefined_name_compares_as_zero() {
        let known = defines(&[]);
        assert!(matches!(
            eval_directive("#if SHADERVERSION >= 70", &known),
            Tri::False
        ));
    }

    #[test]
    fn a_known_value_decides_the_branch() {
        let known = defines(&[("SHADERVERSION", Some(70))]);
        assert!(matches!(
            eval_directive("#if SHADERVERSION >= 70", &known),
            Tri::True
        ));
        assert!(matches!(
            eval_directive("#if SHADERVERSION < 70", &known),
            Tri::False
        ));
    }

    #[test]
    fn a_value_we_cannot_read_stays_unknown() {
        let known = defines(&[("SHADERVERSION", None)]);
        assert!(matches!(
            eval_directive("#if SHADERVERSION >= 70", &known),
            Tri::Unknown
        ));
    }

    #[test]
    fn equality_and_inequality_are_read_too() {
        let known = defines(&[("LIGHTS", Some(2))]);
        assert!(matches!(eval_directive("#if LIGHTS == 2", &known), Tri::True));
        assert!(matches!(eval_directive("#if LIGHTS != 2", &known), Tri::False));
    }

    #[test]
    fn logical_operators_follow_c_precedence() {
        let known = defines(&[
            ("REFLECTION", Some(0)),
            ("NORMALMAP", Some(1)),
            ("BLENDMODE", Some(0)),
        ]);
        assert!(matches!(
            eval_directive("#if REFLECTION && NORMALMAP", &known),
            Tri::False
        ));
        assert!(matches!(
            eval_directive("#if REFLECTION && NORMALMAP || BLENDMODE || CLIPPINGUVS", &known),
            Tri::False
        ));
        assert!(matches!(
            eval_directive("#if (REFLECTION || NORMALMAP) && BLENDMODE == 0", &known),
            Tri::True
        ));
        assert!(matches!(
            eval_directive("#if !(NORMALMAP && SHADERVERSION >= 70)", &known),
            Tri::True
        ));
        assert!(matches!(
            eval_directive("#if NORMALMAP == 1 && !defined(BLENDMODE)", &known),
            Tri::False
        ));
    }

    #[test]
    fn an_unknown_operand_only_taints_what_depends_on_it() {
        let known = defines(&[("LIGHTING", None), ("BLENDMODE", Some(1))]);
        assert!(matches!(
            eval_directive("#if LIGHTING || BLENDMODE", &known),
            Tri::True
        ));
        assert!(matches!(
            eval_directive("#if LIGHTING && BLENDMODE", &known),
            Tri::Unknown
        ));
        assert!(matches!(
            eval_directive("#if LIGHTING && REFLECTION", &known),
            Tri::False
        ));
    }
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
