use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct StatusSnapshot {
    pub speed: f32,
    pub screens: Vec<ScreenStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenStatus {
    pub screen: String,
    pub bg: Option<PathBuf>,
}

pub(crate) fn format_status(snapshot: &StatusSnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"speed=");
    out.extend_from_slice(format_speed(snapshot.speed).as_bytes());
    out.push(b'\n');
    let mut screens: Vec<&ScreenStatus> = snapshot.screens.iter().collect();
    screens.sort_by(|a, b| a.screen.as_bytes().cmp(b.screen.as_bytes()));
    for sc in screens {
        out.extend_from_slice(b"screen=");
        out.extend_from_slice(sc.screen.as_bytes());
        out.extend_from_slice(b" bg=");
        if let Some(p) = &sc.bg {
            out.extend_from_slice(p.as_os_str().as_bytes());
        }
        out.push(b'\n');
    }
    out
}

pub(crate) fn format_speed(value: f32) -> String {
    let v = f64::from(value);
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if v == 0.0 {
        return "0".into();
    }
    let sci = format!("{v:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let x: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&x) {
        let prec = (5 - x).max(0) as usize;
        trim_g(format!("{v:.prec$}"))
    } else {
        let m = trim_g(mantissa.to_string());
        format!("{m}e{}{:02}", if x < 0 { '-' } else { '+' }, x.abs())
    }
}

fn trim_g(mut s: String) -> String {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_matches_cpp_stream_default_format() {
        assert_eq!(format_speed(1.0), "1");
        assert_eq!(format_speed(0.5), "0.5");
        assert_eq!(format_speed(0.25), "0.25");
        assert_eq!(format_speed(2.0), "2");
        assert_eq!(format_speed(1.75), "1.75");
        assert_eq!(format_speed(100.0), "100");
        assert_eq!(format_speed(0.1), "0.1");
        assert_eq!(format_speed(1.0 / 3.0), "0.333333");
        assert_eq!(format_speed(123456.0), "123456");
        assert_eq!(format_speed(1_000_000.0), "1e+06");
        assert_eq!(format_speed(0.0001), "0.0001");
        assert_eq!(format_speed(0.00001), "1e-05");
        assert_eq!(format_speed(0.0), "0");
    }

    #[test]
    fn status_body_single_screen_matches_live_capture() {
        let snap = StatusSnapshot {
            speed: 1.0,
            screens: vec![ScreenStatus {
                screen: "HDMI-A-1".into(),
                bg: Some(PathBuf::from(
                    "/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375",
                )),
            }],
        };
        assert_eq!(
            format_status(&snap),
            b"speed=1\nscreen=HDMI-A-1 bg=/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375\n"
        );
    }

    #[test]
    fn status_screens_sorted_lexicographically_by_bytes() {
        let snap = StatusSnapshot {
            speed: 0.5,
            screens: vec![
                ScreenStatus {
                    screen: "HDMI-A-1".into(),
                    bg: Some(PathBuf::from("/c")),
                },
                ScreenStatus {
                    screen: "DP-2".into(),
                    bg: Some(PathBuf::from("/b")),
                },
                ScreenStatus {
                    screen: "DP-10".into(),
                    bg: None,
                },
            ],
        };
        assert_eq!(
            format_status(&snap),
            b"speed=0.5\nscreen=DP-10 bg=\nscreen=DP-2 bg=/b\nscreen=HDMI-A-1 bg=/c\n"
        );
    }
}
