use std::collections::HashMap;

pub fn relax_hlsl_shapes(source: &str) -> String {
    let widths = declared_widths(source);
    if widths.is_empty() {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        match narrower_target(line).or_else(|| narrowest_constructor(line)) {
            Some(width) => out.push_str(&truncate_wider(line, width, &widths)),
            None => out.push_str(line),
        }
    }
    out
}

fn narrowest_constructor(line: &str) -> Option<usize> {
    let mut narrowest = None;
    for (kind, width) in [("vec2(", 2_usize), ("vec3(", 3)] {
        if line.contains(kind) {
            narrowest = Some(narrowest.map_or(width, |had: usize| had.min(width)));
        }
    }
    narrowest
}

fn width_of(kind: &str) -> Option<usize> {
    match kind {
        "float" => Some(1),
        "vec2" => Some(2),
        "vec3" => Some(3),
        "vec4" => Some(4),
        _ => None,
    }
}

fn declared_widths(source: &str) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for line in source.lines() {
        let text = line.trim_start();
        let mut words = text.split_whitespace();
        let Some(first) = words.next() else { continue };
        let (kind, name) = match first {
            "varying" | "attribute" | "uniform" | "in" | "out" => {
                let Some(kind) = words.next() else { continue };
                let Some(name) = words.next() else { continue };
                (kind, name)
            }
            _ => continue,
        };
        let Some(width) = width_of(kind) else { continue };
        let name = name.trim_end_matches([';', ',', ')']);
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.insert(name.to_owned(), width);
        }
    }
    out
}

fn narrower_target(line: &str) -> Option<usize> {
    let text = line.trim_start();
    let mut words = text.split_whitespace();
    let kind = words.next()?;
    let width = width_of(kind)?;
    if width == 1 {
        return None;
    }
    let name = words.next()?;
    if !name
        .trim_end_matches(['=', ';'])
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    text.contains('=').then_some(width)
}

fn truncate_wider(line: &str, width: usize, widths: &HashMap<String, usize>) -> String {
    let swizzle = match width {
        2 => ".xy",
        3 => ".xyz",
        _ => return line.to_owned(),
    };
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        if !is_word_byte(bytes[at]) {
            out.push(line[at..].chars().next().unwrap_or(' '));
            at += line[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let start = at;
        while at < line.len() && is_word_byte(bytes[at]) {
            at += 1;
        }
        let word = &line[start..at];
        let already_narrowed = line[at..].starts_with('.') || line[at..].starts_with('(');
        let wider = widths.get(word).is_some_and(|w| *w > width);
        out.push_str(word);
        if wider && !already_narrowed {
            out.push_str(swizzle);
        }
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "varying vec4 v_TexCoord;\nvarying vec2 v_Offset;\n";

    #[test]
    fn a_constructor_on_the_line_sets_the_width() {
        let source = "varying vec3 v_TexCoord;\nfloat d = length(v_TexCoord - vec2(0.5));\n";
        let out = relax_hlsl_shapes(source);
        assert!(out.contains("v_TexCoord.xy - vec2(0.5)"), "{out}");
    }

    #[test]
    fn a_plain_length_call_is_left_alone() {
        let source = "varying vec3 v_TexCoord;\nfloat d = length(v_TexCoord);\n";
        assert_eq!(relax_hlsl_shapes(source), source);
    }

    #[test]
    fn a_wide_varying_is_narrowed_to_the_target() {
        let source = format!("{HEADER}vec2 uv = (v_TexCoord * 2.0 - 1.0) + v_Offset;\n");
        let out = relax_hlsl_shapes(&source);
        assert!(out.contains("v_TexCoord.xy * 2.0"), "{out}");
        assert!(!out.contains("v_Offset.xy"), "{out}");
    }

    #[test]
    fn a_varying_that_already_picks_components_is_left_alone() {
        let source = format!("{HEADER}vec2 uv = v_TexCoord.zw + v_Offset;\n");
        let out = relax_hlsl_shapes(&source);
        assert!(out.contains("v_TexCoord.zw"), "{out}");
        assert!(!out.contains(".zw.xy"), "{out}");
    }

    #[test]
    fn a_matching_width_is_not_touched() {
        let source = format!("{HEADER}vec4 c = v_TexCoord * 2.0;\n");
        assert_eq!(relax_hlsl_shapes(&source), source);
    }

    #[test]
    fn a_line_that_declares_nothing_is_left_alone() {
        let source = format!("{HEADER}    gl_FragColor = vec4(v_TexCoord);\n");
        assert_eq!(relax_hlsl_shapes(&source), source);
    }

    #[test]
    fn a_shader_without_declarations_is_returned_as_is() {
        let source = "void main() { vec2 uv = whatever; }\n";
        assert_eq!(relax_hlsl_shapes(source), source);
    }

    #[test]
    fn a_vec3_target_narrows_a_vec4() {
        let source = "varying vec4 v_Colour;\nvec3 rgb = v_Colour * 0.5;\n";
        let out = relax_hlsl_shapes(source);
        assert!(out.contains("v_Colour.xyz * 0.5"), "{out}");
    }
}
