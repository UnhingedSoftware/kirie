const HELPERS: &str = "float we_mod(float a, float b) { return b == 0.0 ? 0.0 : a - b * floor(a / b); }
vec2 we_mod(vec2 a, vec2 b) { return mod(a, b); }
vec2 we_mod(vec2 a, float b) { return mod(a, b); }
vec3 we_mod(vec3 a, vec3 b) { return mod(a, b); }
vec3 we_mod(vec3 a, float b) { return mod(a, b); }
vec4 we_mod(vec4 a, vec4 b) { return mod(a, b); }
vec4 we_mod(vec4 a, float b) { return mod(a, b); }
int we_mod(int a, int b) { return b == 0 ? 0 : a % b; }
uint we_mod(uint a, uint b) { return b == 0u ? 0u : a % b; }
uint we_mod(uint a, int b) { return we_mod(a, uint(b)); }
int we_mod(int a, uint b) { return we_mod(a, int(b)); }
float we_mod(float a, int b) { return we_mod(a, float(b)); }
float we_mod(int a, float b) { return we_mod(float(a), b); }
float we_mod(float a, uint b) { return we_mod(a, float(b)); }
float we_mod(uint a, float b) { return we_mod(float(a), b); }
";

#[must_use]
pub fn rewrite_modulo(source: &str) -> String {
    let mut rewritten = String::with_capacity(source.len() + 64);
    let mut touched = false;
    for line in source.split_inclusive('\n') {
        let (code, done) = rewrite_line(line);
        touched |= done;
        rewritten.push_str(&code);
    }
    if !touched {
        return source.to_owned();
    }

    let at = after_leading_directives(&rewritten);
    let mut out = String::with_capacity(rewritten.len() + HELPERS.len());
    out.push_str(&rewritten[..at]);
    out.push_str(HELPERS);
    out.push_str(&rewritten[at..]);
    out
}

fn rewrite_line(line: &str) -> (String, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with("//") || !line.contains('%') {
        return (line.to_owned(), false);
    }

    let mut code = line.to_owned();
    let mut touched = false;
    while let Some(at) = next_modulo(&code) {
        let Some((left, right)) = operands(&code, at) else {
            break;
        };
        let call = format!(
            "we_mod({}, {})",
            code[left..at].trim(),
            code[at + 1..right].trim()
        );
        code.replace_range(left..right, &call);
        touched = true;
    }
    (code, touched)
}

fn next_modulo(code: &str) -> Option<usize> {
    let bytes = code.as_bytes();
    let stop = code.find("//").unwrap_or(code.len());
    let mut at = 0;
    while at < stop {
        if bytes[at] == b'%' {
            let after_is_assign = bytes.get(at + 1) == Some(&b'=');
            let inside_call = code[..at].ends_with("we_mod(");
            if !after_is_assign && !inside_call {
                return Some(at);
            }
        }
        at += 1;
    }
    None
}

fn operands(code: &str, at: usize) -> Option<(usize, usize)> {
    let left = left_operand(code, at)?;
    let right = right_operand(code, at + 1)?;
    (left < at && right > at + 1).then_some((left, right))
}

fn left_operand(code: &str, at: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut start = skip_space_back(bytes, at);
    loop {
        let primary = primary_start(bytes, start)?;
        let before = skip_space_back(bytes, primary);
        let joins = before > 0 && matches!(bytes[before - 1], b'*' | b'/' | b'%');
        if !joins {
            return Some(primary);
        }
        start = skip_space_back(bytes, before - 1);
    }
}

fn primary_start(bytes: &[u8], end: usize) -> Option<usize> {
    if end == 0 {
        return None;
    }
    let mut at = end;
    if bytes[at - 1] == b')' || bytes[at - 1] == b']' {
        at = opening(bytes, at - 1)?;
    }
    while at > 0 && (is_word(bytes[at - 1]) || bytes[at - 1] == b'.') {
        at -= 1;
        if at > 0 && (bytes[at] == b'.') && !is_word(bytes[at - 1]) {
            break;
        }
    }
    (at < end).then_some(at)
}

fn opening(bytes: &[u8], close: usize) -> Option<usize> {
    let (open, shut) = if bytes[close] == b')' {
        (b'(', b')')
    } else {
        (b'[', b']')
    };
    let mut depth = 0i32;
    let mut at = close;
    loop {
        if bytes[at] == shut {
            depth += 1;
        } else if bytes[at] == open {
            depth -= 1;
            if depth == 0 {
                return Some(at);
            }
        }
        if at == 0 {
            return None;
        }
        at -= 1;
    }
}

fn right_operand(code: &str, from: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut at = from;
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    while at < bytes.len() && matches!(bytes[at], b'-' | b'+' | b'!' | b'~') {
        at += 1;
    }
    if at >= bytes.len() {
        return None;
    }
    if bytes[at] == b'(' {
        return closing(bytes, at).map(|shut| shut + 1);
    }
    let start = at;
    while at < bytes.len() && (is_word(bytes[at]) || bytes[at] == b'.') {
        at += 1;
    }
    if at < bytes.len() && (bytes[at] == b'(' || bytes[at] == b'[') {
        at = closing(bytes, at)? + 1;
    }
    (at > start).then_some(at)
}

fn closing(bytes: &[u8], open: usize) -> Option<usize> {
    let (shut, other) = if bytes[open] == b'(' {
        (b')', b'(')
    } else {
        (b']', b'[')
    };
    let mut depth = 0i32;
    for (at, byte) in bytes.iter().enumerate().skip(open) {
        if *byte == other {
            depth += 1;
        } else if *byte == shut {
            depth -= 1;
            if depth == 0 {
                return Some(at);
            }
        }
    }
    None
}

fn skip_space_back(bytes: &[u8], from: usize) -> usize {
    let mut at = from;
    while at > 0 && bytes[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    at
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
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
    fn a_float_modulo_becomes_a_call() {
        let out = rewrite_modulo("void main() { uint a = frequency % RESOLUTION; }\n");
        assert!(out.contains("we_mod(frequency, RESOLUTION)"), "{out}");
        assert!(
            out.contains("float we_mod(float a, float b)"),
            "helpers are injected"
        );
    }

    #[test]
    fn a_shader_without_modulo_is_untouched() {
        let src = "void main() { float a = b * c; }\n";
        assert_eq!(rewrite_modulo(src), src);
    }

    #[test]
    fn the_left_hand_multiplicative_chain_stays_together() {
        let out = rewrite_modulo("void main() { float x = a * b % c; }\n");
        assert!(out.contains("we_mod(a * b, c)"), "{out}");
    }

    #[test]
    fn a_parenthesised_operand_is_kept_whole() {
        let out = rewrite_modulo("void main() { uint x = (a + 1) % RES; }\n");
        assert!(out.contains("we_mod((a + 1), RES)"), "{out}");
    }

    #[test]
    fn a_chain_of_modulos_nests_left_to_right() {
        let out = rewrite_modulo("void main() { float x = a % b % c; }\n");
        assert!(out.contains("we_mod(we_mod(a, b), c)"), "{out}");
    }

    #[test]
    fn a_call_on_the_right_is_taken_whole() {
        let out = rewrite_modulo("void main() { float x = a % floor(b + 1.0); }\n");
        assert!(out.contains("we_mod(a, floor(b + 1.0))"), "{out}");
    }

    #[test]
    fn preprocessor_and_comment_lines_are_left_alone() {
        let src = "#if RESOLUTION % 2\n// y = x % 4\n";
        assert_eq!(rewrite_modulo(src), src);
    }

    #[test]
    fn a_trailing_comment_is_not_rewritten() {
        let out = rewrite_modulo("void main() { float x = a % b; } // c % d\n");
        assert!(out.contains("we_mod(a, b)"), "{out}");
        assert!(out.contains("// c % d"), "{out}");
    }

    #[test]
    fn a_compound_assignment_is_left_for_the_compiler() {
        let src = "void main() { x %= 2; }\n";
        assert_eq!(rewrite_modulo(src), src);
    }

    #[test]
    fn the_helpers_land_after_the_version_line() {
        let out = rewrite_modulo("#version 450\nprecision highp float;\nvoid main() { float x = a % b; }\n");
        let version = out.find("#version").unwrap_or(usize::MAX);
        let helpers = out.find("float we_mod").unwrap_or(0);
        assert!(version < helpers, "{out}");
    }
}
