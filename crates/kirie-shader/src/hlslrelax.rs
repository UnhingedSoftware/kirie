use std::collections::HashMap;

pub fn relax_hlsl_shapes(source: &str) -> String {
    if std::env::var_os("KIRIE_HLSL_RELAX").is_none() {
        return source.to_owned();
    }
    narrow_wide_values(source)
}

fn narrow_wide_values(source: &str) -> String {
    let widths = declared_widths(source);
    let matrices = declared_matrices(source);
    if widths.is_empty() {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        if touches_a_matrix(line, &matrices) {
            out.push_str(line);
            continue;
        }
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

fn declared_matrices(source: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in source.lines() {
        let mut words = line.split_whitespace();
        while let Some(word) = words.next() {
            if matches!(word, "mat2" | "mat3" | "mat4") {
                if let Some(name) = words.next() {
                    let name = name.trim_end_matches([';', ',', ')', '(']);
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        out.insert(name.to_owned());
                    }
                }
                break;
            }
        }
    }
    out
}

fn touches_a_matrix(line: &str, matrices: &std::collections::HashSet<String>) -> bool {
    if line.contains("mat2") || line.contains("mat3") || line.contains("mat4") {
        return true;
    }
    matrices.iter().any(|name| {
        line.match_indices(name.as_str()).any(|(at, _)| {
            let before = line[..at].chars().next_back();
            let after = line[at + name.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
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

pub(crate) fn declared_widths(source: &str) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for line in source.lines() {
        let text = line.trim_start();
        if text.starts_with("//") {
            continue;
        }
        let words = word_tokens(text);
        for pair in words.windows(2) {
            let (kind, after) = pair[0];
            let (name, _) = pair[1];
            let Some(width) = width_of(kind) else { continue };
            if text[after..].starts_with('(') || width_of(name).is_some() {
                continue;
            }
            out.insert(name.to_owned(), width);
        }
    }
    out
}

fn word_tokens(text: &str) -> Vec<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if !is_word_byte(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < text.len() && is_word_byte(bytes[at]) {
            at += 1;
        }
        out.push((&text[start..at], at));
    }
    out
}

fn declared_name(line: &str) -> Option<String> {
    let text = line.trim_start();
    let mut words = text.split_whitespace();
    let kind = words.next()?;
    width_of(kind)?;
    let name = words.next()?.trim_end_matches(['=', ';', ',']);
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| name.to_owned())
}

fn narrower_target(line: &str) -> Option<usize> {
    if let Some(width) = swizzled_target(line) {
        return Some(width);
    }
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

fn swizzled_target(line: &str) -> Option<usize> {
    let (left, _) = line.split_once('=')?;
    if left.contains("==") || left.contains('<') || left.contains('>') || left.contains('!') {
        return None;
    }
    let target = left.trim();
    let (_, swizzle) = target.rsplit_once('.')?;
    let swizzle = swizzle.trim();
    let letters = swizzle.len();
    if letters == 0 || letters > 3 {
        return None;
    }
    swizzle.chars().all(|c| "xyzwrgba".contains(c)).then_some(letters)
}

pub(crate) fn truncate_wider(line: &str, width: usize, widths: &HashMap<String, usize>) -> String {
    let swizzle = match width {
        2 => ".xy",
        3 => ".xyz",
        _ => return line.to_owned(),
    };
    let declared_here = declared_name(line);
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
        let wider = widths.get(word).is_some_and(|w| *w > width) && declared_here.as_deref() != Some(word);
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
    fn a_line_with_a_matrix_is_left_alone() {
        let source = "uniform mat4 g_Proj;\nvarying vec4 v_Point;\nout.xyz = (g_Proj * v_Point).xyw;\n";
        assert_eq!(narrow_wide_values(source), source);
    }

    #[test]
    fn the_name_being_declared_is_never_swizzled() {
        let source = "varying vec4 noise;\nvec2 noise = fract(vec2(1.0));\n";
        let out = narrow_wide_values(source);
        assert!(!out.contains("noise.xy ="), "{out}");
    }

    #[test]
    fn a_swizzled_target_narrows_a_local() {
        let source = "vec4 sample_we = fetch();\nout_FragColor.xyz = sample_we * mask;\n";
        let out = narrow_wide_values(source);
        assert!(out.contains("sample_we.xyz * mask"), "{out}");
    }

    #[test]
    fn a_comparison_is_not_a_target() {
        let source = "vec4 c = fetch();\nif (c.x == 1.0) { }\n";
        assert_eq!(narrow_wide_values(source), source);
    }

    #[test]
    fn a_constructor_on_the_line_sets_the_width() {
        let source = "varying vec3 v_TexCoord;\nfloat d = length(v_TexCoord - vec2(0.5));\n";
        let out = narrow_wide_values(source);
        assert!(out.contains("v_TexCoord.xy - vec2(0.5)"), "{out}");
    }

    #[test]
    fn a_plain_length_call_is_left_alone() {
        let source = "varying vec3 v_TexCoord;\nfloat d = length(v_TexCoord);\n";
        assert_eq!(narrow_wide_values(source), source);
    }

    #[test]
    fn a_wide_varying_is_narrowed_to_the_target() {
        let source = format!("{HEADER}vec2 uv = (v_TexCoord * 2.0 - 1.0) + v_Offset;\n");
        let out = narrow_wide_values(&source);
        assert!(out.contains("v_TexCoord.xy * 2.0"), "{out}");
        assert!(!out.contains("v_Offset.xy"), "{out}");
    }

    #[test]
    fn a_varying_that_already_picks_components_is_left_alone() {
        let source = format!("{HEADER}vec2 uv = v_TexCoord.zw + v_Offset;\n");
        let out = narrow_wide_values(&source);
        assert!(out.contains("v_TexCoord.zw"), "{out}");
        assert!(!out.contains(".zw.xy"), "{out}");
    }

    #[test]
    fn a_matching_width_is_not_touched() {
        let source = format!("{HEADER}vec4 c = v_TexCoord * 2.0;\n");
        assert_eq!(narrow_wide_values(&source), source);
    }

    #[test]
    fn a_line_that_declares_nothing_is_left_alone() {
        let source = format!("{HEADER}    gl_FragColor = vec4(v_TexCoord);\n");
        assert_eq!(narrow_wide_values(&source), source);
    }

    #[test]
    fn a_shader_without_declarations_is_returned_as_is() {
        let source = "void main() { vec2 uv = whatever; }\n";
        assert_eq!(narrow_wide_values(source), source);
    }

    #[test]
    fn a_vec3_target_narrows_a_vec4() {
        let source = "varying vec4 v_Colour;\nvec3 rgb = v_Colour * 0.5;\n";
        let out = narrow_wide_values(source);
        assert!(out.contains("v_Colour.xyz * 0.5"), "{out}");
    }

    #[test]
    fn a_layout_qualified_varying_is_measured() {
        let widths = declared_widths("layout(location = 0) smooth out highp vec4 v_PointerUV;\n");
        assert_eq!(widths.get("v_PointerUV"), Some(&4));
    }

    #[test]
    fn a_comment_declares_nothing() {
        let widths = declared_widths("// uniform vec4 g_Fake;\n");
        assert!(widths.is_empty(), "{widths:?}");
    }

    #[test]
    fn function_parameters_are_measured() {
        let widths = declared_widths("float roundedBox(vec2 CurPosition, vec3 Size, float r) {\n");
        assert_eq!(widths.get("CurPosition"), Some(&2));
        assert_eq!(widths.get("Size"), Some(&3));
        assert_eq!(widths.get("r"), Some(&1));
    }

    #[test]
    fn a_constructor_is_not_a_declaration() {
        let widths = declared_widths("gl_FragColor = vec4(albedo, 1.0);\n");
        assert!(!widths.contains_key("albedo"), "{widths:?}");
    }
}
