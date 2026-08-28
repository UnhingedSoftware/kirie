use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    Float,
    Int,
    Uint,
    Bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ty {
    base: Base,
    size: u8,
}

impl Ty {
    fn ctor(self) -> &'static str {
        match (self.base, self.size) {
            (Base::Float, 1) => "float",
            (Base::Float, 2) => "vec2",
            (Base::Float, 3) => "vec3",
            (Base::Float, 4) => "vec4",
            (Base::Int, 1) => "int",
            (Base::Int, 2) => "ivec2",
            (Base::Int, 3) => "ivec3",
            (Base::Int, 4) => "ivec4",
            (Base::Uint, 1) => "uint",
            (Base::Uint, 2) => "uvec2",
            (Base::Uint, 3) => "uvec3",
            (Base::Uint, 4) => "uvec4",
            (Base::Bool, 1) => "bool",
            (Base::Bool, 2) => "bvec2",
            (Base::Bool, 3) => "bvec3",
            _ => "vec4",
        }
    }
}

fn parse_ty(tok: &str) -> Option<Ty> {
    let (base, size) = match tok {
        "float" => (Base::Float, 1),
        "vec2" => (Base::Float, 2),
        "vec3" => (Base::Float, 3),
        "vec4" => (Base::Float, 4),
        "int" => (Base::Int, 1),
        "ivec2" => (Base::Int, 2),
        "ivec3" => (Base::Int, 3),
        "ivec4" => (Base::Int, 4),
        "uint" => (Base::Uint, 1),
        "uvec2" => (Base::Uint, 2),
        "uvec3" => (Base::Uint, 3),
        "uvec4" => (Base::Uint, 4),
        "bool" => (Base::Bool, 1),
        "bvec2" => (Base::Bool, 2),
        "bvec3" => (Base::Bool, 3),
        "bvec4" => (Base::Bool, 4),
        _ => return None,
    };
    Some(Ty { base, size })
}

struct TypeEnv {
    vars: HashMap<String, Ty>,
    funcs: HashMap<String, (Vec<Option<Ty>>, Option<Ty>)>,
}

pub fn coerce_shapes(src: &str) -> String {
    let env = build_env(src);
    let mut out = String::with_capacity(src.len() + 64);
    for line in src.lines() {
        let rewritten = coerce_line(line, &env);
        out.push_str(&rewritten);
        out.push('\n');
    }
    out
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn build_env(src: &str) -> TypeEnv {
    let mut vars: HashMap<String, Ty> = HashMap::new();
    let mut funcs: HashMap<String, (Vec<Option<Ty>>, Option<Ty>)> = HashMap::new();

    for raw in src.lines() {
        let line = raw.trim();
        if let Some((name, sig, ret)) = parse_function_sig(line) {
            funcs.entry(name).or_insert((sig, ret));
            continue;
        }
        register_var_decl(line, &mut vars);
    }
    TypeEnv { vars, funcs }
}

fn register_var_decl(line: &str, vars: &mut HashMap<String, Ty>) {
    let mut s = line.trim();
    for q in [
        "in ", "out ", "uniform ", "flat ", "smooth ", "const ", "highp ", "lowp ", "mediump ",
    ] {
        while let Some(rest) = s.trim_start().strip_prefix(q) {
            s = rest.trim_start();
        }
    }
    let s = s.trim().trim_end_matches(';').trim();
    let head = s.split('=').next().unwrap_or(s).trim();
    if head.contains('(') || head.contains('{') || head.contains('[') {
        return;
    }
    let mut it = head.split_whitespace();
    let (Some(ty_tok), Some(name)) = (it.next(), it.next()) else {
        return;
    };
    if it.next().is_some() {
        return;
    }
    if let Some(ty) = parse_ty(ty_tok)
        && name.bytes().all(is_ident)
    {
        vars.insert(name.to_string(), ty);
    }
}

fn parse_function_sig(line: &str) -> Option<(String, Vec<Option<Ty>>, Option<Ty>)> {
    let open = line.find('(')?;
    let head = line[..open].trim();
    let mut ht = head.split_whitespace();
    let ret_tok = ht.next()?;
    let name = ht.next()?;
    if ht.next().is_some() || !name.bytes().all(is_ident) {
        return None;
    }
    let ret = parse_ty(ret_tok);
    let close = line[open..].find(')')? + open;
    let tail = line[close + 1..].trim_start();
    if !(tail.starts_with('{') || tail.starts_with(';') || tail.is_empty()) {
        return None;
    }
    let params_str = &line[open + 1..close];
    let mut params = Vec::new();
    if params_str.trim() != "void" && !params_str.trim().is_empty() {
        for p in params_str.split(',') {
            let ptok = p.split_whitespace().find(|t| {
                !matches!(
                    *t,
                    "const" | "in" | "out" | "inout" | "highp" | "lowp" | "mediump"
                )
            });
            params.push(ptok.and_then(parse_ty));
        }
    }
    Some((name.to_string(), params, ret))
}

fn infer(expr: &str, env: &TypeEnv) -> Option<Ty> {
    let e = expr.trim();
    let e = e.strip_prefix(['-', '+']).unwrap_or(e).trim();
    if let Some(open) = e.find('(')
        && e.ends_with(')')
        && paren_balanced_span(e, open)
    {
        let callee = e[..open].trim();
        if callee == "texture" || callee == "textureLod" {
            return Some(Ty {
                base: Base::Float,
                size: 4,
            });
        }
        if let Some((_, ret)) = env.funcs.get(callee) {
            return *ret;
        }
        return None;
    }
    let (base_ident, swizzle) = match e.split_once('.') {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (e, None),
    };
    if !base_ident.bytes().all(is_ident) || base_ident.is_empty() {
        return None;
    }
    let ty = *env.vars.get(base_ident)?;
    match swizzle {
        None => Some(ty),
        Some(sw) => {
            if !sw.bytes().all(|c| b"xyzwrgbastpq".contains(&c)) || sw.is_empty() || sw.len() > 4 {
                return None;
            }
            Some(Ty {
                base: ty.base,
                size: sw.len() as u8,
            })
        }
    }
}

fn paren_balanced_span(e: &str, open: usize) -> bool {
    let bytes = e.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i == bytes.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

fn coerce_line(line: &str, env: &TypeEnv) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with("//") {
        return line.to_string();
    }
    let indent_len = line.len() - trimmed.len();
    let (indent, code) = line.split_at(indent_len);
    let with_calls = coerce_calls(code, env);
    let with_assign = coerce_for_init(&coerce_assignment(&with_calls, env), env);
    format!("{indent}{with_assign}")
}

fn coerce_for_init(code: &str, env: &TypeEnv) -> String {
    let Some(fpos) = code.find("for") else {
        return code.to_string();
    };
    let before_ok = fpos
        .checked_sub(1)
        .map(|b| !is_ident(code.as_bytes()[b]))
        .unwrap_or(true);
    let after = code[fpos + 3..].trim_start();
    if !before_ok || !after.starts_with('(') {
        return code.to_string();
    }
    let open = fpos + 3 + (code[fpos + 3..].find('(').unwrap());
    let Some(semi_rel) = code[open + 1..].find(';') else {
        return code.to_string();
    };
    let init_start = open + 1;
    let init_end = init_start + semi_rel;
    let init = &code[init_start..init_end];
    let Some(eq) = find_plain_assign(init) else {
        return code.to_string();
    };
    let lhs = init[..eq].trim();
    let rhs = init[eq + 1..].trim();
    let (Some(lt), Some(rt)) = (lhs_type(lhs, env), infer(rhs, env)) else {
        return code.to_string();
    };
    if lt == rt {
        return code.to_string();
    }
    let Some(new_rhs) = coerce_expr_to(rhs, rt, lt) else {
        return code.to_string();
    };
    format!("{}{lhs} = {new_rhs}{}", &code[..init_start], &code[init_end..])
}

fn coerce_assignment(code: &str, env: &TypeEnv) -> String {
    let Some(semi) = code.rfind(';') else {
        return code.to_string();
    };
    let stmt = &code[..semi];
    let after = &code[semi..];
    let Some(eq) = find_plain_assign(stmt) else {
        return code.to_string();
    };
    let lhs = stmt[..eq].trim();
    let rhs = stmt[eq + 1..].trim();
    if rhs.is_empty() {
        return code.to_string();
    }

    let lhs_ty = lhs_type(lhs, env);
    let Some(lt) = lhs_ty else {
        return code.to_string();
    };
    let Some(rt) = infer(rhs, env) else {
        return code.to_string();
    };
    if lt == rt {
        return code.to_string();
    }

    let new_rhs = coerce_expr_to(rhs, rt, lt);
    let Some(new_rhs) = new_rhs else {
        return code.to_string();
    };
    format!("{lhs} = {new_rhs}{after}")
}

fn lhs_type(lhs: &str, env: &TypeEnv) -> Option<Ty> {
    let mut it = lhs.split_whitespace();
    let first = it.next()?;
    if let Some(ty) = parse_ty(first) {
        return Some(ty);
    }
    infer(lhs, env)
}

fn coerce_expr_to(expr: &str, from: Ty, to: Ty) -> Option<String> {
    if from.size == to.size && from.base != to.base && from.size == 1 {
        return Some(format!("{}({})", to.ctor(), expr));
    }
    if from.base != to.base {
        return None;
    }
    if from.size > to.size {
        let sw = &"xyzw"[..to.size as usize];
        return Some(format!("({expr}).{sw}"));
    }
    if from.size == 1 && to.size > 1 {
        return Some(format!("{}({})", to.ctor(), expr));
    }
    None
}

fn find_plain_assign(stmt: &str) -> Option<usize> {
    let bytes = stmt.as_bytes();
    let mut depth = 0i32;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = i.checked_sub(1).map(|p| bytes[p]);
                let next = bytes.get(i + 1).copied();
                if next == Some(b'=') {
                    continue;
                }
                if matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/')) {
                    continue;
                }
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

fn coerce_calls(code: &str, env: &TypeEnv) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        if is_ident(bytes[i]) && (i == 0 || !is_ident(bytes[i - 1])) {
            let start = i;
            let mut j = i;
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                let name = &code[start..j];
                if let Some((args_span, end)) = arg_list_span(code, j) {
                    let handled = rewrite_call(name, &code[args_span.0..args_span.1], env);
                    if let Some(new_args) = handled {
                        out.push_str(name);
                        out.push('(');
                        out.push_str(&coerce_calls(&new_args, env));
                        out.push(')');
                        i = end;
                        continue;
                    }
                }
            }
            out.push_str(&code[start..j]);
            i = j;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn arg_list_span(code: &str, open: usize) -> Option<((usize, usize), usize)> {
    let mut depth = 0i32;
    for (i, &b) in code.as_bytes().iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(((open + 1, i), i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn rewrite_call(name: &str, args: &str, env: &TypeEnv) -> Option<String> {
    let parts = split_top_commas(args);
    let want: Vec<Option<Ty>> = match name {
        "texture" | "textureLod" => {
            let mut w = vec![None; parts.len()];
            if parts.len() >= 2 {
                w[1] = Some(Ty {
                    base: Base::Float,
                    size: 2,
                });
            }
            w
        }
        "mix" => {
            let t0 = parts.first().and_then(|a| infer(a, env));
            let t1 = parts.get(1).and_then(|a| infer(a, env));
            if let (Some(a), Some(b)) = (t0, t1)
                && a.base == b.base
                && a.size != b.size
            {
                let target = Ty {
                    base: a.base,
                    size: a.size.min(b.size),
                };
                let mut w = vec![None; parts.len()];
                w[0] = Some(target);
                w[1] = Some(target);
                w
            } else {
                return None;
            }
        }
        _ => {
            let (sig, _) = env.funcs.get(name)?;
            sig.iter()
                .cloned()
                .chain(std::iter::repeat(None))
                .take(parts.len())
                .collect()
        }
    };

    let mut changed = false;
    let mut rebuilt: Vec<String> = Vec::with_capacity(parts.len());
    for (idx, part) in parts.iter().enumerate() {
        let expected = want.get(idx).copied().flatten();
        if let Some(exp) = expected
            && let Some(cur) = infer(part, env)
            && cur != exp
            && let Some(fixed) = coerce_expr_to(part.trim(), cur, exp)
        {
            rebuilt.push(fixed);
            changed = true;
        } else {
            rebuilt.push(part.trim().to_string());
        }
    }
    changed.then(|| rebuilt.join(", "))
}

fn split_top_commas(args: &str) -> Vec<String> {
    if args.trim().is_empty() {
        return Vec::new();
    }
    let bytes = args.as_bytes();
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut last = 0;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(args[last..i].to_string());
                last = i + 1;
            }
            _ => {}
        }
    }
    parts.push(args[last..].to_string());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_vec4_to_vec2_truncates() {
        let src =
            "out vec4 v_TexCoord;\nout vec2 v_NoiseCoord;\nvoid main() {\nv_NoiseCoord = v_TexCoord;\n}\n";
        let got = coerce_shapes(src);
        assert!(got.contains("v_NoiseCoord = (v_TexCoord).xy;"), "{got}");
    }

    #[test]
    fn scalar_float_to_int_wraps() {
        let src = "void main() {\nfloat iterations = 3.0;\nint i = -iterations;\n}\n";
        let got = coerce_shapes(src);
        assert!(got.contains("int i = int(-iterations);"), "{got}");
    }

    #[test]
    fn mix_unifies_operand_widths() {
        let src = "void main() {\nvec4 albedo = vec4(0.0);\nvec3 newAlbedo = vec3(0.0);\nfloat mask = 1.0;\nalbedo.rgb = mix(albedo, newAlbedo, mask);\n}\n";
        let got = coerce_shapes(src);
        assert!(got.contains("mix((albedo).xyz, newAlbedo, mask)"), "{got}");
    }

    #[test]
    fn user_func_arg_truncated() {
        let src = "vec2 rotateVec2(vec2 v, float a) { return v; }\nvec4 v_TexCoord;\nvoid main() {\nvec2 c = rotateVec2(v_TexCoord, 1.0);\n}\n";
        let got = coerce_shapes(src);
        assert!(got.contains("rotateVec2((v_TexCoord).xy, 1.0)"), "{got}");
    }

    #[test]
    fn texture_coord_forced_to_vec2() {
        let src = "vec4 v_TexCoord;\nvoid main() {\nvec4 c = texture(g_Tex, v_TexCoord);\n}\n";
        let got = coerce_shapes(src);
        assert!(got.contains("texture(g_Tex, (v_TexCoord).xy)"), "{got}");
    }

    #[test]
    fn matching_types_untouched() {
        let src = "void main() {\nvec3 a = vec3(0.0);\nvec3 b = a;\n}\n";
        let got = coerce_shapes(src);
        assert!(got.contains("vec3 b = a;"), "{got}");
    }
}
