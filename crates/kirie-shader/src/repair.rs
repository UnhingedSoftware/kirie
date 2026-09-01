pub fn repair_conversion(source: &str, diagnostic: &str) -> Option<String> {
    if let Some(mended) = repair_not_on_a_float(source, diagnostic) {
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
    let (body, tail) = match ends_with.strip_suffix(';') {
        Some(body) => (body, ";"),
        None => return None,
    };
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
        if matches!(before, Some(b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/')) {
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
}
