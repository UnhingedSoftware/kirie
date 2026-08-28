mod backend;
pub mod host;
mod webkit_sys;

pub use backend::WebviewBackend;

#[must_use]
pub fn file_url(path: &std::path::Path) -> String {
    use std::path::Component;

    let mut url = String::from("file://");
    for comp in path.components() {
        match comp {
            Component::RootDir => { /* leading '/' emitted below per-segment */ }
            Component::Prefix(_) => { /* Windows prefixes: not a target platform */ }
            Component::CurDir => {}
            Component::ParentDir => {
                url.push('/');
                url.push_str("..");
            }
            Component::Normal(seg) => {
                url.push('/');
                encode_segment(&seg.to_string_lossy(), &mut url);
            }
        }
    }
    if url == "file://" {
        url.push('/');
    }
    url
}

fn encode_segment(seg: &str, out: &mut String) {
    for &b in seg.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn encodes_spaces_and_keeps_slashes() {
        let url = file_url(Path::new("/home/a b/My Wallpaper/index.html"));
        assert_eq!(url, "file:///home/a%20b/My%20Wallpaper/index.html");
    }

    #[test]
    fn keeps_unreserved() {
        let url = file_url(Path::new("/a-b_c.d~e/index.html"));
        assert_eq!(url, "file:///a-b_c.d~e/index.html");
    }
}
