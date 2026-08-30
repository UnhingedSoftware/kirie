const MAT2: &str = "mat2 inverse(mat2 m) {
    float d = m[0][0] * m[1][1] - m[1][0] * m[0][1];
    return mat2(m[1][1], -m[0][1], -m[1][0], m[0][0]) / d;
}
";

const MAT3: &str = "mat3 inverse(mat3 m) {
    float a00 = m[0][0], a01 = m[0][1], a02 = m[0][2];
    float a10 = m[1][0], a11 = m[1][1], a12 = m[1][2];
    float a20 = m[2][0], a21 = m[2][1], a22 = m[2][2];
    float b01 = a22 * a11 - a12 * a21;
    float b11 = -a22 * a10 + a12 * a20;
    float b21 = a21 * a10 - a11 * a20;
    float det = a00 * b01 + a01 * b11 + a02 * b21;
    return mat3(
        b01, (-a22 * a01 + a02 * a21), (a12 * a01 - a02 * a11),
        b11, (a22 * a00 - a02 * a20), (-a12 * a00 + a02 * a10),
        b21, (-a21 * a00 + a01 * a20), (a11 * a00 - a01 * a10)
    ) / det;
}
";

const MAT4: &str = "mat4 inverse(mat4 m) {
    float a00 = m[0][0], a01 = m[0][1], a02 = m[0][2], a03 = m[0][3];
    float a10 = m[1][0], a11 = m[1][1], a12 = m[1][2], a13 = m[1][3];
    float a20 = m[2][0], a21 = m[2][1], a22 = m[2][2], a23 = m[2][3];
    float a30 = m[3][0], a31 = m[3][1], a32 = m[3][2], a33 = m[3][3];
    float b00 = a00 * a11 - a01 * a10;
    float b01 = a00 * a12 - a02 * a10;
    float b02 = a00 * a13 - a03 * a10;
    float b03 = a01 * a12 - a02 * a11;
    float b04 = a01 * a13 - a03 * a11;
    float b05 = a02 * a13 - a03 * a12;
    float b06 = a20 * a31 - a21 * a30;
    float b07 = a20 * a32 - a22 * a30;
    float b08 = a20 * a33 - a23 * a30;
    float b09 = a21 * a32 - a22 * a31;
    float b10 = a21 * a33 - a23 * a31;
    float b11 = a22 * a33 - a23 * a32;
    float det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    return mat4(
        a11 * b11 - a12 * b10 + a13 * b09, a02 * b10 - a01 * b11 - a03 * b09,
        a31 * b05 - a32 * b04 + a33 * b03, a22 * b04 - a21 * b05 - a23 * b03,
        a12 * b08 - a10 * b11 - a13 * b07, a00 * b11 - a02 * b08 + a03 * b07,
        a32 * b02 - a30 * b05 - a33 * b01, a20 * b05 - a22 * b02 + a23 * b01,
        a10 * b10 - a11 * b08 + a13 * b06, a01 * b08 - a00 * b10 - a03 * b06,
        a30 * b04 - a31 * b02 + a33 * b00, a21 * b02 - a20 * b04 - a23 * b00,
        a11 * b07 - a10 * b09 - a12 * b06, a00 * b09 - a01 * b07 + a02 * b06,
        a31 * b01 - a30 * b03 - a32 * b00, a20 * b03 - a21 * b01 + a22 * b00
    ) / det;
}
";

#[must_use]
pub fn shadow_builtin_inverse(source: &str) -> String {
    if !mentions_inverse(source) {
        return source.to_owned();
    }

    let mut injected = String::new();
    for (dimension, body) in [(2_usize, MAT2), (3, MAT3), (4, MAT4)] {
        if !defines_inverse(source, dimension) {
            injected.push_str(body);
        }
    }
    if injected.is_empty() {
        return source.to_owned();
    }

    let at = after_leading_directives(source);
    let mut out = String::with_capacity(source.len() + injected.len());
    out.push_str(&source[..at]);
    out.push_str(&injected);
    out.push_str(&source[at..]);
    out
}

fn mentions_inverse(source: &str) -> bool {
    source
        .match_indices("inverse")
        .any(|(at, _)| !neighbour_is_word(source, at.wrapping_sub(1)) && follows_with_call(source, at + 7))
}

fn neighbour_is_word(source: &str, at: usize) -> bool {
    source
        .as_bytes()
        .get(at)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn follows_with_call(source: &str, from: usize) -> bool {
    source
        .get(from..)
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('('))
}

fn defines_inverse(source: &str, dimension: usize) -> bool {
    let kind = format!("mat{dimension}");
    source.match_indices(&kind).any(|(at, _)| {
        let rest = source.get(at + kind.len()..).unwrap_or("").trim_start();
        rest.starts_with("inverse")
            && rest
                .get("inverse".len()..)
                .map(str::trim_start)
                .is_some_and(|tail| tail.starts_with('('))
    })
}

fn after_leading_directives(source: &str) -> usize {
    let mut at = 0;
    for line in source.split_inclusive('\n') {
        let text = line.trim_start();
        let carry_on = text.is_empty()
            || text.starts_with("//")
            || text.starts_with("#version")
            || text.starts_with("#extension")
            || text.starts_with("#pragma")
            || text.starts_with("#line")
            || text.starts_with("precision ");
        if !carry_on {
            break;
        }
        at += line.len();
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shader_without_inverse_is_left_alone() {
        let source = "#version 450\nvoid main() { gl_Position = vec4(0.0); }\n";
        assert_eq!(shadow_builtin_inverse(source), source);
    }

    #[test]
    fn a_call_gets_every_overload_the_shader_lacks() {
        let source = "#version 450\nvoid main() { mat4 m = inverse(mvp); }\n";
        let out = shadow_builtin_inverse(source);
        assert!(out.contains("mat2 inverse(mat2 m)"));
        assert!(out.contains("mat3 inverse(mat3 m)"));
        assert!(out.contains("mat4 inverse(mat4 m)"));
    }

    #[test]
    fn an_overload_the_shader_defines_is_not_duplicated() {
        let source = "#version 450\nmat3 inverse(mat3 m) { return m; }\nvoid main() { inverse(x); }\n";
        let out = shadow_builtin_inverse(source);
        assert_eq!(out.matches("mat3 inverse(mat3").count(), 1);
        assert!(out.contains("mat4 inverse(mat4 m)"));
    }

    #[test]
    fn the_definitions_land_after_the_leading_directives() {
        let source = "#version 450\n#extension GL_ARB_x : enable\nvoid main() { inverse(m); }\n";
        let out = shadow_builtin_inverse(source);
        let extension = out.find("#extension").unwrap_or(usize::MAX);
        let injected = out.find("mat2 inverse").unwrap_or(0);
        assert!(extension < injected, "{out}");
    }

    #[test]
    fn a_word_that_merely_ends_in_inverse_is_not_a_call() {
        let source = "#version 450\nvoid main() { float x = g_ModelMatrixInverse(y); }\n";
        assert_eq!(shadow_builtin_inverse(source), source);
    }
}
