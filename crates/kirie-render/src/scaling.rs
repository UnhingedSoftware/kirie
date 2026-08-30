use crate::error::RenderError;

static FOCUS_X: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FOCUS_Y: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_focus(x: f32, y: f32) {
    let keep = |value: f32| {
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    FOCUS_X.store(keep(x).to_bits(), std::sync::atomic::Ordering::Relaxed);
    FOCUS_Y.store(keep(y).to_bits(), std::sync::atomic::Ordering::Relaxed);
}

#[must_use]
pub fn focus() -> (f32, f32) {
    (
        f32::from_bits(FOCUS_X.load(std::sync::atomic::Ordering::Relaxed)),
        f32::from_bits(FOCUS_Y.load(std::sync::atomic::Ordering::Relaxed)),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScalingMode {
    #[default]
    Default,
    Fit,
    Fill,
    Stretch,
}

impl ScalingMode {
    pub fn from_cli(value: &str) -> Result<Self, RenderError> {
        match value {
            "default" => Ok(Self::Default),
            "fit" => Ok(Self::Fit),
            "fill" => Ok(Self::Fill),
            "stretch" => Ok(Self::Stretch),
            other => Err(RenderError::BadScalingMode(other.to_owned())),
        }
    }

    #[must_use]
    pub fn as_cli_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Stretch => "stretch",
        }
    }

    #[must_use]
    pub fn uv_window(self, content: (u32, u32), viewport: (u32, u32)) -> UvWindow {
        let (cw, ch) = (content.0 as f32, content.1 as f32);
        let (vw, vh) = (viewport.0 as f32, viewport.1 as f32);
        if cw <= 0.0 || ch <= 0.0 || vw <= 0.0 || vh <= 0.0 {
            return UvWindow::FULL;
        }

        let wide = vh * cw;
        let tall = vw * ch;

        match self {
            Self::Stretch => UvWindow::FULL,
            Self::Fill => {
                if wide > tall {
                    UvWindow::with_u(u_range(cw, ch, vw, vh))
                } else if tall > wide {
                    UvWindow::with_v(v_range(cw, ch, vw, vh))
                } else {
                    UvWindow::FULL
                }
            }
            Self::Fit => {
                if wide < tall {
                    UvWindow::with_u(u_range(cw, ch, vw, vh))
                } else if tall < wide {
                    UvWindow::with_v(v_range(cw, ch, vw, vh))
                } else {
                    UvWindow::FULL
                }
            }
            Self::Default => {
                let mut window = UvWindow::FULL;
                if (vh > vw && cw >= ch) || (vw > vh && ch > cw) {
                    (window.u0, window.u1) = u_range(cw, ch, vw, vh);
                }
                if (vw > vh && cw >= ch) || (vh > vw && ch > cw) {
                    (window.v0, window.v1) = v_range(cw, ch, vw, vh);
                }
                window
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClampMode {
    #[default]
    Clamp,
    Border,
    Repeat,
}

impl ClampMode {
    pub fn from_cli(value: &str) -> Result<Self, RenderError> {
        match value {
            "clamp" => Ok(Self::Clamp),
            "border" => Ok(Self::Border),
            "repeat" => Ok(Self::Repeat),
            other => Err(RenderError::BadClampMode(other.to_owned())),
        }
    }

    #[must_use]
    pub fn as_cli_str(self) -> &'static str {
        match self {
            Self::Clamp => "clamp",
            Self::Border => "border",
            Self::Repeat => "repeat",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvWindow {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

impl UvWindow {
    pub const FULL: Self = Self {
        u0: 0.0,
        v0: 0.0,
        u1: 1.0,
        v1: 1.0,
    };

    fn with_u((u0, u1): (f32, f32)) -> Self {
        Self { u0, u1, ..Self::FULL }
    }

    fn with_v((v0, v1): (f32, f32)) -> Self {
        Self { v0, v1, ..Self::FULL }
    }

    #[must_use]
    pub fn slid(self, (x, y): (f32, f32)) -> Self {
        let room = |low: f32, high: f32, by: f32| {
            let span = high - low;
            let free = 1.0 - span;
            if free <= f32::EPSILON {
                return (low, high);
            }
            let moved = (free / 2.0) * by.clamp(-1.0, 1.0);
            let low = (low + moved).clamp(0.0, 1.0 - span);
            (low, low + span)
        };
        let (u0, u1) = room(self.u0, self.u1, x);
        let (v0, v1) = room(self.v0, self.v1, y);
        Self { u0, u1, v0, v1 }
    }

    #[must_use]
    pub fn strip_corners(&self) -> [[f32; 2]; 4] {
        [
            [self.u0, self.v0],
            [self.u0, self.v1],
            [self.u1, self.v0],
            [self.u1, self.v1],
        ]
    }
}

fn u_range(cw: f32, ch: f32, vw: f32, vh: f32) -> (f32, f32) {
    let half = (vw * ch) / (2.0 * vh * cw);
    (0.5 - half, 0.5 + half)
}

fn v_range(cw: f32, ch: f32, vw: f32, vh: f32) -> (f32, f32) {
    let half = (vh * cw) / (2.0 * vw * ch);
    (0.5 - half, 0.5 + half)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: (u32, u32) = (1920, 1080);
    const HD_PORTRAIT: (u32, u32) = (1080, 1920);
    const SQUARE: (u32, u32) = (1024, 1024);

    fn window(mode: ScalingMode, content: (u32, u32), viewport: (u32, u32)) -> UvWindow {
        mode.uv_window(content, viewport)
    }

    #[test]
    fn stretch_is_always_full_window() {
        for content in [HD, HD_PORTRAIT, SQUARE] {
            for viewport in [(1920, 1080), (1080, 1920), (1000, 1000), (2560, 1080)] {
                assert_eq!(window(ScalingMode::Stretch, content, viewport), UvWindow::FULL);
            }
        }
    }

    #[test]
    fn fill_matching_aspect_is_full() {
        assert_eq!(window(ScalingMode::Fill, HD, (3840, 2160)), UvWindow::FULL);
        assert_eq!(window(ScalingMode::Fill, SQUARE, (512, 512)), UvWindow::FULL);
    }

    #[test]
    fn fill_wider_viewport_crops_v() {
        let w = window(ScalingMode::Fill, HD, (2560, 1080));
        assert_eq!((w.u0, w.u1), (0.0, 1.0));
        assert_eq!((w.v0, w.v1), (0.125, 0.875));
    }

    #[test]
    fn fill_portrait_viewport_crops_u() {
        let w = window(ScalingMode::Fill, HD, (1080, 1920));
        assert_eq!((w.u0, w.u1), (0.5 - 81.0 / 512.0, 0.5 + 81.0 / 512.0));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
    }

    #[test]
    fn fill_square_viewport_on_landscape_crops_u() {
        let w = window(ScalingMode::Fill, HD, (1000, 1000));
        assert_eq!((w.u0, w.u1), (0.21875, 0.78125));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
    }

    #[test]
    fn fill_square_viewport_on_portrait_crops_v() {
        let w = window(ScalingMode::Fill, HD_PORTRAIT, (1000, 1000));
        assert_eq!((w.u0, w.u1), (0.0, 1.0));
        assert_eq!((w.v0, w.v1), (0.21875, 0.78125));
    }

    #[test]
    fn fit_matching_aspect_is_full() {
        assert_eq!(window(ScalingMode::Fit, HD, (3840, 2160)), UvWindow::FULL);
    }

    #[test]
    fn fit_wider_viewport_overscans_u() {
        let w = window(ScalingMode::Fit, HD, (2560, 1080));
        let two_thirds = 2.0f32 / 3.0;
        assert_eq!((w.u0, w.u1), (0.5 - two_thirds, 0.5 + two_thirds));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
        assert!(w.u0 < 0.0 && w.u1 > 1.0);
    }

    #[test]
    fn fit_portrait_viewport_overscans_v() {
        let w = window(ScalingMode::Fit, HD, (1080, 1920));
        let half = (1920.0f32 * 1920.0) / (2.0 * 1080.0 * 1080.0);
        assert_eq!((w.u0, w.u1), (0.0, 1.0));
        assert_eq!((w.v0, w.v1), (0.5 - half, 0.5 + half));
        assert!(w.v0 < 0.0 && w.v1 > 1.0);
    }

    #[test]
    fn fit_square_content_on_landscape_overscans_u() {
        let w = window(ScalingMode::Fit, SQUARE, (2048, 1024));
        assert_eq!((w.u0, w.u1), (-0.5, 1.5));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
    }

    #[test]
    fn default_landscape_viewport_landscape_content_adjusts_v() {
        assert_eq!(window(ScalingMode::Default, HD, (1920, 1080)), UvWindow::FULL);

        let w = window(ScalingMode::Default, HD, (2560, 1080));
        assert_eq!((w.u0, w.u1), (0.0, 1.0));
        assert_eq!((w.v0, w.v1), (0.125, 0.875));

        let w = window(ScalingMode::Default, HD, (1024, 768));
        let two_thirds = 2.0f32 / 3.0;
        assert_eq!((w.u0, w.u1), (0.0, 1.0));
        assert_eq!((w.v0, w.v1), (0.5 - two_thirds, 0.5 + two_thirds));
    }

    #[test]
    fn default_portrait_viewport_landscape_content_adjusts_u() {
        let w = window(ScalingMode::Default, HD, (1080, 1920));
        assert_eq!((w.u0, w.u1), (0.5 - 81.0 / 512.0, 0.5 + 81.0 / 512.0));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
    }

    #[test]
    fn default_landscape_viewport_portrait_content_adjusts_u() {
        let w = window(ScalingMode::Default, HD_PORTRAIT, (1920, 1080));
        let half = (1920.0f32 * 1920.0) / (2.0 * 1080.0 * 1080.0);
        assert_eq!((w.u0, w.u1), (0.5 - half, 0.5 + half));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
    }

    #[test]
    fn default_portrait_viewport_portrait_content_adjusts_v() {
        assert_eq!(
            window(ScalingMode::Default, HD_PORTRAIT, (1080, 1920)),
            UvWindow::FULL
        );

        let w = window(ScalingMode::Default, HD_PORTRAIT, (1080, 2520));
        assert_eq!((w.u0, w.u1), (0.0, 1.0));
        assert_eq!((w.v0, w.v1), (0.5 - 0.65625, 0.5 + 0.65625));
    }

    #[test]
    fn default_square_viewport_touches_nothing() {
        for content in [HD, HD_PORTRAIT, SQUARE] {
            assert_eq!(
                window(ScalingMode::Default, content, (1000, 1000)),
                UvWindow::FULL
            );
        }
    }

    #[test]
    fn default_square_content_counts_as_landscape() {
        let w = window(ScalingMode::Default, SQUARE, (1080, 1920));
        assert_eq!((w.u0, w.u1), (0.5 - 0.28125, 0.5 + 0.28125));
        assert_eq!((w.v0, w.v1), (0.0, 1.0));
    }

    #[test]
    fn zero_dimensions_yield_full_window() {
        for mode in [
            ScalingMode::Default,
            ScalingMode::Fit,
            ScalingMode::Fill,
            ScalingMode::Stretch,
        ] {
            assert_eq!(mode.uv_window((0, 0), (1920, 1080)), UvWindow::FULL);
            assert_eq!(mode.uv_window(HD, (0, 0)), UvWindow::FULL);
            assert_eq!(mode.uv_window((1920, 0), (0, 1080)), UvWindow::FULL);
        }
    }

    #[test]
    fn strip_corners_map_window_to_quad() {
        let w = UvWindow {
            u0: 0.125,
            v0: 0.25,
            u1: 0.875,
            v1: 0.75,
        };
        assert_eq!(
            w.strip_corners(),
            [[0.125, 0.25], [0.125, 0.75], [0.875, 0.25], [0.875, 0.75]]
        );
    }

    #[test]
    fn cli_scaling_round_trip() {
        for (s, mode) in [
            ("default", ScalingMode::Default),
            ("fit", ScalingMode::Fit),
            ("fill", ScalingMode::Fill),
            ("stretch", ScalingMode::Stretch),
        ] {
            assert_eq!(ScalingMode::from_cli(s).unwrap(), mode);
            assert_eq!(mode.as_cli_str(), s);
        }
        assert!(matches!(
            ScalingMode::from_cli("Fit"),
            Err(RenderError::BadScalingMode(_))
        ));
        assert!(matches!(
            ScalingMode::from_cli(""),
            Err(RenderError::BadScalingMode(_))
        ));
    }

    #[test]
    fn cli_clamp_round_trip() {
        for (s, mode) in [
            ("clamp", ClampMode::Clamp),
            ("border", ClampMode::Border),
            ("repeat", ClampMode::Repeat),
        ] {
            assert_eq!(ClampMode::from_cli(s).unwrap(), mode);
            assert_eq!(mode.as_cli_str(), s);
        }
        assert!(matches!(
            ClampMode::from_cli("edge"),
            Err(RenderError::BadClampMode(_))
        ));
    }

    #[test]
    fn cli_defaults_match_compat_cli() {
        assert_eq!(ScalingMode::default(), ScalingMode::Default);
        assert_eq!(ClampMode::default(), ClampMode::Clamp);
    }
}
