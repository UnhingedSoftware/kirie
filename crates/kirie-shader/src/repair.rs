pub fn repair_conversion(source: &str, diagnostic: &str) -> Option<String> {
    if let Some(mended) = repair_not_on_a_float(source, diagnostic) {
        return Some(mended);
    }
    if let Some(mended) = repair_operand_mismatch(source, diagnostic) {
        return Some(mended);
    }
    if let Some(mended) = repair_texture_coordinate(source, diagnostic) {
        return Some(mended);
    }
    if let Some(mended) = repair_builtin_overload(source, diagnostic) {
        return Some(mended);
    }
    if let Some(mended) = repair_written_input(source, diagnostic) {
        return Some(mended);
    }
    let (line_no, want) = first_conversion(diagnostic)?;
    let mut lines: Vec<&str> = source.split_inclusive('\n').collect();
    let at = line_no.checked_sub(1)?;
    let line = *lines.get(at)?;
    let fixed = wrap_right_hand_side(line, want)?;
    let held = fixed;
    lines[at] = held.as_str();
    Some(lines.concat())
}

fn repair_written_input(source: &str, diagnostic: &str) -> Option<String> {
    let report = diagnostic
        .lines()
        .find(|line| line.contains("l-value required") && line.contains("can't modify shader input"))?;
    let (_, after) = report.split_once('"')?;
    let (name, _) = after.split_once('"')?;
    if name.is_empty() {
        return None;
    }
    let held = format!("{name}_we_in");
    let mut lines: Vec<String> = source.split_inclusive('\n').map(str::to_owned).collect();
    let at = lines.iter().position(|line| is_input_declaration(line, name))?;
    let kind = input_type(&lines[at])?;
    lines[at] = replace_word(&lines[at], name, &held);
    let main = lines.iter().position(|line| line.contains("void main("))?;
    let opens = lines[main..].iter().position(|line| line.contains('{'))? + main;
    lines.insert(opens + 1, format!("{kind} {name} = {held};\n"));
    Some(lines.concat())
}

fn is_input_declaration(line: &str, name: &str) -> bool {
    let text = line.trim_start();
    if !text.ends_with(&format!("{name};\n")) && !text.trim_end().ends_with(&format!("{name};")) {
        return false;
    }
    text.split_whitespace().any(|word| word == "in") || text.starts_with("varying")
}

fn input_type(line: &str) -> Option<String> {
    let mut kind = None;
    for word in line.split_whitespace() {
        if matches!(word, "float" | "vec2" | "vec3" | "vec4") {
            kind = Some(word.to_owned());
        }
    }
    kind
}

fn replace_word(line: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + from.len()..].chars().next();
        let bounded = !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        out.push_str(&rest[..at]);
        out.push_str(if bounded { to } else { from });
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

fn repair_builtin_overload(source: &str, diagnostic: &str) -> Option<String> {
    let report = diagnostic
        .lines()
        .find(|line| line.contains("no matching overloaded function"))?;
    let line_no = numbered_line(report)?;
    let (_, after) = report.split_once("error: '")?;
    let (name, _) = after.split_once('\'')?;
    if name == "texture" || name.is_empty() {
        return None;
    }
    let mut lines: Vec<&str> = source.split_inclusive('\n').collect();
    let at = line_no.checked_sub(1)?;
    let line = *lines.get(at)?;
    let widths = crate::hlslrelax::declared_widths(source);
    let mended = narrow_call_arguments(line, name, &widths)?;
    lines[at] = mended.as_str();
    Some(lines.concat())
}

fn narrow_call_arguments(
    line: &str,
    name: &str,
    widths: &std::collections::HashMap<String, usize>,
) -> Option<String> {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut mended = false;
    let needle = format!("{name}(");
    while let Some(at) = rest.find(&needle) {
        let before_char = rest[..at].chars().next_back();
        if before_char.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            let split = at + needle.len();
            out.push_str(&rest[..split]);
            rest = &rest[split..];
            continue;
        }
        out.push_str(&rest[..at]);
        let open = at + name.len();
        let Some(close) = matching_paren(rest, open) else {
            out.push_str(&rest[at..]);
            return mended.then_some(out);
        };
        let args = split_arguments(&rest[open + 1..close]);
        let narrowest = args
            .iter()
            .filter_map(|arg| argument_width(arg, widths))
            .filter(|width| *width > 1)
            .min();
        let held: Vec<String> = args
            .iter()
            .map(|arg| {
                let mut text = arg.clone();
                if let Some(promoted) = whole_number_as_float(arg) {
                    mended = true;
                    text = promoted;
                }
                let Some(width) = narrowest else { return text };
                match argument_width(&text, widths) {
                    Some(had) if had > width => {
                        mended = true;
                        let swizzle = if width == 2 { ".xy" } else { ".xyz" };
                        format!("({text}){swizzle}")
                    }
                    Some(1) => {
                        mended = true;
                        format!("vec{width}({text})")
                    }
                    _ => text,
                }
            })
            .collect();
        let rebuilt = held.join(", ");
        out.push_str(name);
        out.push('(');
        out.push_str(&rebuilt);
        out.push(')');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    mended.then_some(out)
}

fn whole_number_as_float(arg: &str) -> Option<String> {
    let arg = arg.trim();
    let digits = arg.strip_prefix('-').unwrap_or(arg);
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then(|| format!("{arg}.0"))
}

fn argument_width(arg: &str, widths: &std::collections::HashMap<String, usize>) -> Option<usize> {
    let arg = arg.trim();
    for (kind, width) in [("vec2(", 2_usize), ("vec3(", 3), ("vec4(", 4)] {
        if arg.starts_with(kind) {
            return Some(width);
        }
    }
    if let Some((base, swizzle)) = arg.rsplit_once('.')
        && !swizzle.is_empty()
        && swizzle.len() <= 4
        && swizzle.chars().all(|c| "xyzwrgba".contains(c))
        && !base.is_empty()
    {
        return Some(swizzle.len());
    }
    if arg.parse::<f64>().is_ok() {
        return Some(1);
    }
    if arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return widths.get(arg).copied();
    }
    widest_name_in(arg, widths)
}

fn widest_name_in(expr: &str, widths: &std::collections::HashMap<String, usize>) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut widest = None;
    let mut at = 0;
    while at < expr.len() {
        if !(bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
            at += 1;
            continue;
        }
        let start = at;
        while at < expr.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }
        let name = &expr[start..at];
        let Some(declared) = widths.get(name).copied() else {
            continue;
        };
        let here = match expr[at..].strip_prefix('.') {
            Some(rest) => {
                let swizzle: String = rest.chars().take_while(|c| "xyzwrgba".contains(*c)).collect();
                if swizzle.is_empty() {
                    declared
                } else {
                    swizzle.len()
                }
            }
            None => declared,
        };
        widest = Some(widest.map_or(here, |had: usize| had.max(here)));
    }
    widest
}

fn repair_texture_coordinate(source: &str, diagnostic: &str) -> Option<String> {
    let line_no = diagnostic.lines().find_map(|line| {
        (line.contains("'texture'") && line.contains("no matching overloaded function"))
            .then(|| numbered_line(line))
            .flatten()
    })?;
    let mut lines: Vec<&str> = source.split_inclusive('\n').collect();
    let at = line_no.checked_sub(1)?;
    let line = *lines.get(at)?;
    let mended = pick_texture_components(line)?;
    lines[at] = mended.as_str();
    Some(lines.concat())
}

fn pick_texture_components(line: &str) -> Option<String> {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut mended = false;
    while let Some(at) = rest.find("texture(") {
        let (before, tail) = rest.split_at(at);
        out.push_str(before);
        let open = at + "texture".len();
        let Some(close) = matching_paren(rest, open) else {
            out.push_str(tail);
            return mended.then_some(out);
        };
        let inside = &rest[open + 1..close];
        let mut args = split_arguments(inside);
        let wanted = args.first().and_then(|sampler| sampler_components(sampler));
        match (wanted, args.get_mut(1)) {
            (Some(width), Some(coord)) if !already_picks(coord) => {
                let swizzle = if width == 2 { ".xy" } else { ".xyz" };
                *coord = format!("({}){swizzle}", coord.trim());
                mended = true;
            }
            _ => {}
        }
        out.push_str("texture(");
        out.push_str(&args.join(", "));
        out.push(')');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    mended.then_some(out)
}

fn sampler_components(sampler: &str) -> Option<usize> {
    if sampler.contains("sampler2DArray") || sampler.contains("samplerCube") {
        return Some(3);
    }
    if sampler.contains("sampler3D") {
        return Some(3);
    }
    sampler.contains("sampler2D").then_some(2)
}

fn already_picks(coord: &str) -> bool {
    let coord = coord.trim();
    let Some((_, swizzle)) = coord.rsplit_once('.') else {
        return false;
    };
    !swizzle.is_empty() && swizzle.len() <= 4 && swizzle.chars().all(|c| "xyzwrgba".contains(c))
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 0_usize;
    for (at, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_arguments(inside: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    for (at, byte) in inside.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(inside[start..at].trim().to_owned());
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push(inside[start..].trim().to_owned());
    out
}

fn repair_operand_mismatch(source: &str, diagnostic: &str) -> Option<String> {
    let report = diagnostic
        .lines()
        .find(|line| line.contains("wrong operand types") && line.contains("left-hand operand"))?;
    let line_no = numbered_line(report)?;
    let (left, right) = report.split_once(" and a right operand of type ")?;
    let (_, left) = left.split_once("left-hand operand of type ")?;
    let narrower = width_of(left)?.min(width_of(right)?);
    if narrower == 0 || width_of(left) == width_of(right) {
        return None;
    }
    let mut lines: Vec<&str> = source.split_inclusive('\n').collect();
    let at = line_no.checked_sub(1)?;
    let line = *lines.get(at)?;
    let widths = crate::hlslrelax::declared_widths(source);
    let mended = crate::hlslrelax::truncate_wider(line, narrower, &widths);
    if mended == line {
        return None;
    }
    lines[at] = mended.as_str();
    Some(lines.concat())
}

fn repair_not_on_a_float(source: &str, diagnostic: &str) -> Option<String> {
    let line_no = diagnostic.lines().find_map(|line| {
        (line.contains("'!'") && line.contains("wrong operand type") && line.contains("float"))
            .then(|| numbered_line(line))
            .flatten()
    })?;
    let mut lines: Vec<&str> = source.split_inclusive('\n').collect();
    let at = line_no.checked_sub(1)?;
    let line = *lines.get(at)?;
    let mended = zero_test_for_not(line)?;
    lines[at] = mended.as_str();
    Some(lines.concat())
}

fn zero_test_for_not(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'!' || bytes.get(at + 1) == Some(&b'=') {
            at += 1;
            continue;
        }
        let mut start = at + 1;
        while start < bytes.len() && bytes[start] == b' ' {
            start += 1;
        }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end == start {
            at += 1;
            continue;
        }
        let name = &line[start..end];
        return Some(format!(
            "{}({name} == 0.0 ? 1.0 : 0.0){}",
            &line[..at],
            &line[end..]
        ));
    }
    None
}

fn first_conversion(diagnostic: &str) -> Option<(usize, usize)> {
    for line in diagnostic.lines() {
        if !line.contains("cannot convert from") {
            continue;
        }
        let Some(line_no) = numbered_line(line) else {
            continue;
        };
        let (_, to) = line.split_once(" to ")?;
        return Some((line_no, width_of(to)?));
    }
    None
}

fn numbered_line(line: &str) -> Option<usize> {
    let mut rest = line;
    while let Some(at) = rest.find(':') {
        let after = &rest[at + 1..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && after[digits.len()..].starts_with(':') {
            return digits.parse().ok();
        }
        rest = after;
    }
    None
}

fn width_of(kind: &str) -> Option<usize> {
    for (needle, width) in [
        ("2-component vector of float", 2_usize),
        ("3-component vector of float", 3),
        ("4-component vector of float", 4),
    ] {
        if kind.contains(needle) {
            return Some(width);
        }
    }
    kind.contains("float").then_some(1)
}

fn wrap_right_hand_side(line: &str, want: usize) -> Option<String> {
    let kind = match want {
        1 => "float",
        2 => "vec2",
        3 => "vec3",
        4 => "vec4",
        _ => return None,
    };
    let at = assignment_at(line)?;
    let (left, rest) = line.split_at(at);
    let right = rest.strip_prefix('=')?;
    let ends_with = right.trim_end();
    let (body, tail) = (ends_with.strip_suffix(';')?, ";");
    let body = body.trim();
    if body.is_empty() || body.starts_with(kind) {
        return None;
    }
    let newline = if line.ends_with('\n') { "\n" } else { "" };
    Some(format!("{left}= {kind}({body}){tail}{newline}"))
}

fn assignment_at(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'=' {
            continue;
        }
        let before = at.checked_sub(1).map(|b| bytes[b]);
        let after = bytes.get(at + 1).copied();
        if after == Some(b'=') {
            return None;
        }
        if matches!(
            before,
            Some(b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/')
        ) {
            return None;
        }
        return Some(at);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "void main() {\n    vec2 uv = noiseValue;\n    float m = someVec2;\n}\n";

    #[test]
    fn a_not_on_a_float_becomes_a_zero_test() {
        let source = "void main() {\n    float m = pick ? ! horizontal : horizontal;\n}\n";
        let diagnostic = "shaderc: pass.vert:2: error: '!' :  wrong operand type no operation '!' exists that takes an operand of type  temp highp float";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("(horizontal == 0.0 ? 1.0 : 0.0)"), "{out}");
    }

    #[test]
    fn a_not_equals_is_never_rewritten() {
        let source = "void main() {\n    if (a != b) { }\n}\n";
        let diagnostic = "shaderc: pass.vert:2: error: '!' :  wrong operand type no operation '!' exists that takes an operand of type  temp highp float";
        assert_eq!(repair_conversion(source, diagnostic), None);
    }

    #[test]
    fn a_float_becomes_the_vector_the_line_wants() {
        let diagnostic = "shaderc: pass.frag:2: error: '=' : cannot convert from ' global highp float' to ' temp highp 2-component vector of float'";
        let out = repair_conversion(SOURCE, diagnostic).expect("a repair");
        assert!(out.contains("vec2 uv = vec2(noiseValue);"), "{out}");
    }

    #[test]
    fn a_vector_narrows_to_the_float_the_line_wants() {
        let diagnostic = "shaderc: pass.frag:3: error: '=' : cannot convert from ' temp highp 2-component vector of float' to ' temp highp float'";
        let out = repair_conversion(SOURCE, diagnostic).expect("a repair");
        assert!(out.contains("float m = float(someVec2);"), "{out}");
    }

    #[test]
    fn a_diagnostic_about_something_else_is_left_alone() {
        let diagnostic = "shaderc: pass.frag:2: error: '-' : wrong operand types";
        assert_eq!(repair_conversion(SOURCE, diagnostic), None);
    }

    #[test]
    fn a_comparison_is_never_treated_as_an_assignment() {
        let source = "void main() {\n    if (a == b) { }\n}\n";
        let diagnostic = "shaderc: pass.frag:2: error: '=' : cannot convert from ' global highp float' to ' temp highp 2-component vector of float'";
        assert_eq!(repair_conversion(source, diagnostic), None);
    }

    #[test]
    fn a_line_already_wrapped_is_not_wrapped_again() {
        let source = "void main() {\n    vec2 uv = vec2(noiseValue);\n}\n";
        let diagnostic = "shaderc: pass.frag:2: error: '=' : cannot convert from ' global highp float' to ' temp highp 2-component vector of float'";
        assert_eq!(repair_conversion(source, diagnostic), None);
    }

    #[test]
    fn a_wide_operand_narrows_to_its_partner() {
        let source =
            "varying vec4 v_TexCoord;\nuniform vec2 u_center;\nvec2 uv = v_TexCoord * 2.0 - u_center;\n";
        let diagnostic = "shaderc: pass.frag:3: error: '-' :  wrong operand types: no operation '-' exists that takes a left-hand operand of type ' temp highp 4-component vector of float' and a right operand of type ' temp highp 2-component vector of float'";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("v_TexCoord.xy * 2.0"), "{out}");
    }

    #[test]
    fn matching_operand_widths_are_left_alone() {
        let source = "varying vec4 a;\nvarying vec4 b;\nvec4 c = a - b;\n";
        let diagnostic = "shaderc: pass.frag:3: error: '-' :  wrong operand types: no operation '-' exists that takes a left-hand operand of type ' temp highp 4-component vector of float' and a right operand of type ' temp highp 4-component vector of float'";
        assert_eq!(repair_conversion(source, diagnostic), None);
    }

    #[test]
    fn a_wide_texture_coordinate_is_narrowed() {
        let source = "void main() {\nvec4 c = texture(sampler2D(t_img, t_smp), v_TexCoord);\n}\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'texture' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(
            out.contains("texture(sampler2D(t_img, t_smp), (v_TexCoord).xy)"),
            "{out}"
        );
    }

    #[test]
    fn a_coordinate_that_already_picks_is_left_alone() {
        let source = "void main() {\nvec4 c = texture(sampler2D(t_img, t_smp), v_TexCoord.xy);\n}\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'texture' : no matching overloaded function found";
        assert_eq!(repair_conversion(source, diagnostic), None);
    }

    #[test]
    fn a_cube_sampler_keeps_three_components() {
        let source = "void main() {\nvec4 c = texture(samplerCube(t_img, t_smp), v_Dir);\n}\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'texture' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("(v_Dir).xyz"), "{out}");
    }

    #[test]
    fn a_biased_lookup_keeps_its_third_argument() {
        let source = "void main() {\nvec4 c = texture(sampler2D(t_img, t_smp), v_TexCoord, 1.0);\n}\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'texture' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("(v_TexCoord).xy, 1.0)"), "{out}");
    }

    #[test]
    fn a_builtin_call_narrows_its_wider_argument() {
        let source = "varying vec4 a;\nvarying vec2 b;\nvec2 e = step(a, b);\n";
        let diagnostic = "shaderc: pass.frag:3: error: 'step' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("step((a).xy, b)"), "{out}");
    }

    #[test]
    fn a_scalar_does_not_narrow_the_vectors() {
        let source = "varying vec3 a;\nvec3 e = max(a, 0.0);\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'max' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("max(a, vec3(0.0))"), "{out}");
    }

    #[test]
    fn a_suffixed_name_is_not_the_call() {
        let source = "varying vec4 a;\nvarying vec2 b;\nvec2 e = mystep(a, b);\n";
        let diagnostic = "shaderc: pass.frag:3: error: 'step' : no matching overloaded function found";
        assert_eq!(repair_conversion(source, diagnostic), None);
    }

    #[test]
    fn an_integer_literal_becomes_a_float() {
        let source = "varying vec2 a;\nvec2 e = step(a, 1);\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'step' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("step(a, vec2(1.0))"), "{out}");
    }

    #[test]
    fn a_float_broadcasts_to_the_vector_width() {
        let source = "varying vec2 a;\nvec2 e = step(a, 1.0);\n";
        let diagnostic = "shaderc: pass.frag:2: error: 'step' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("step(a, vec2(1.0))"), "{out}");
    }

    #[test]
    fn a_scalar_broadcasts_to_the_vector_width() {
        let source = "varying vec4 a;\nvec4 e = step(a, 1);\n";
        let diagnostic = "pass.frag:2: error: 'step' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("step(a, vec4(1.0))"), "{out}");
    }

    #[test]
    fn an_expression_takes_the_width_of_its_widest_name() {
        let source = "varying vec4 v_uvTex;\nvec2 c = step(abs(floor(v_uvTex)) + 0.001, 1);\n";
        let diagnostic = "pass.frag:2: error: 'step' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("vec4(1.0)"), "{out}");
    }

    #[test]
    fn a_swizzle_beats_the_declaration() {
        let source = "varying vec4 v;\nvec2 c = step(v.x + 1.0, 1);\n";
        let diagnostic = "pass.frag:2: error: 'step' : no matching overloaded function found";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(!out.contains("vec4("), "{out}");
    }

    #[test]
    fn a_written_input_gets_a_local_copy() {
        let source =
            "layout(location = 0) in highp vec2 v_TexCoord;\nvoid main() {\n v_TexCoord.x = 1.0;\n}\n";
        let diagnostic =
            "pass.frag:3: error: 'assign' :  l-value required \"v_TexCoord\" (can't modify shader input)";
        let out = repair_conversion(source, diagnostic).expect("a repair");
        assert!(out.contains("in highp vec2 v_TexCoord_we_in;"), "{out}");
        assert!(out.contains("vec2 v_TexCoord = v_TexCoord_we_in;"), "{out}");
        assert!(out.contains(" v_TexCoord.x = 1.0;"), "{out}");
    }
}
