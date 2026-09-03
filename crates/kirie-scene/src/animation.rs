use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::value::coerce_f64;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Handle {
    pub enabled: bool,
    pub x: f32,
    pub y: f32,
}

impl Handle {
    fn parse(v: Option<&Value>, default_x: f32) -> Self {
        let Some(Value::Object(o)) = v else {
            return Handle {
                enabled: false,
                x: default_x,
                y: 0.0,
            };
        };
        Handle {
            enabled: o.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            x: o.get("x").and_then(coerce_f64).map_or(default_x, |f| f as f32),
            y: o.get("y").and_then(coerce_f64).map_or(0.0, |f| f as f32),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub frame: f32,
    pub value: f32,
    pub front: Handle,
    pub back: Handle,
    pub step: bool,
}

impl Keyframe {
    fn parse(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(Keyframe {
            frame: o.get("frame").and_then(coerce_f64)? as f32,
            value: o.get("value").and_then(coerce_f64).unwrap_or(0.0) as f32,
            front: Handle::parse(o.get("front"), 1.0),
            back: Handle::parse(o.get("back"), -1.0),
            step: o.get("step").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayMode {
    Single,
    Loop,
    Mirror,
    TimeOfDay,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimationEvent {
    pub frame: f32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropertyAnimation {
    pub channels: Vec<Vec<Keyframe>>,
    pub fps: f32,
    pub length: f32,
    pub mode: PlayMode,
    pub wraploop: bool,
    pub startpaused: bool,
    pub name: Option<String>,
    pub relative: bool,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub events: Vec<AnimationEvent>,
}

impl PropertyAnimation {
    pub fn parse(property: &Map<String, Value>) -> Option<Self> {
        let anim = property.get("animation")?.as_object()?;
        let mut channels = Vec::new();
        for i in 0.. {
            let key = format!("c{i}");
            match anim.get(&key) {
                Some(Value::Array(keys)) => {
                    let mut parsed: Vec<Keyframe> = keys.iter().filter_map(Keyframe::parse).collect();
                    parsed.sort_by(|a, b| a.frame.total_cmp(&b.frame));
                    channels.push(parsed);
                }
                _ => break,
            }
        }
        if channels.is_empty() || channels.iter().all(Vec::is_empty) {
            return None;
        }
        let options = anim.get("options").and_then(Value::as_object);
        let opt = |k: &str| options.and_then(|o| o.get(k));
        let mode = match opt("mode").and_then(Value::as_str).unwrap_or("single") {
            "loop" => PlayMode::Loop,
            "mirror" => PlayMode::Mirror,
            "timeofday" => PlayMode::TimeOfDay,
            _ => PlayMode::Single,
        };
        let last_frame = channels
            .iter()
            .filter_map(|c| c.last())
            .map(|k| k.frame)
            .fold(0.0f32, f32::max);
        let length = opt("length")
            .and_then(coerce_f64)
            .map(|f| f as f32)
            .filter(|f| *f > 0.0)
            .unwrap_or(last_frame.max(1.0));
        let wraploop = match opt("wraploop") {
            Some(Value::Bool(b)) => *b,
            _ => mode == PlayMode::TimeOfDay,
        };
        let keys_of = |v: Option<&Value>| -> Vec<String> {
            match v {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|i| i.get("key").and_then(Value::as_str).map(str::to_owned))
                    .collect(),
                _ => Vec::new(),
            }
        };
        Some(PropertyAnimation {
            channels,
            fps: opt("fps")
                .and_then(coerce_f64)
                .map(|f| f as f32)
                .filter(|f| *f > 0.0)
                .unwrap_or(30.0),
            length,
            mode,
            wraploop,
            startpaused: opt("startpaused").and_then(Value::as_bool).unwrap_or(false),
            name: opt("name").and_then(Value::as_str).map(str::to_owned),
            relative: property.get("relative").and_then(Value::as_bool).unwrap_or(false),
            parent: opt("parent")
                .and_then(|p| p.get("key"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            children: keys_of(opt("children")),
            events: match opt("events") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|e| {
                        Some(AnimationEvent {
                            frame: e.get("frame").and_then(coerce_f64)? as f32,
                            name: e.get("name")?.as_str()?.to_owned(),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            },
        })
    }

    pub fn sample(&self, channel: usize, frame: f32) -> Option<f32> {
        let keys = self.channels.get(channel)?;
        let first = keys.first()?;
        let last = keys.last()?;
        if frame <= first.frame {
            return Some(first.value);
        }
        if frame >= last.frame {
            if self.wraploop && self.length > last.frame && !last.step {
                if frame >= self.length {
                    return Some(first.value);
                }
                let half = (self.length - last.frame) * 0.5;
                let (c1x, c1y) = if last.front.enabled {
                    (last.frame + last.front.x * half, last.value + last.front.y)
                } else {
                    (last.frame, last.value)
                };
                let (c2x, c2y) = if first.front.enabled {
                    (self.length - first.front.x * half, first.value - first.front.y)
                } else {
                    (self.length, first.value)
                };
                return Some(bezier_at(
                    frame,
                    [last.frame, last.value],
                    [c1x, c1y],
                    [c2x, c2y],
                    [self.length, first.value],
                ));
            }
            return Some(last.value);
        }
        let i = keys.partition_point(|k| k.frame <= frame);
        let g = keys[i - 1];
        let a = keys[i];
        if a.step {
            return Some(g.value);
        }
        if a.frame <= g.frame {
            return Some(a.value);
        }
        let half = (a.frame - g.frame) * 0.5;
        let (c1x, c1y) = if g.front.enabled {
            (g.frame + g.front.x * half, g.value + g.front.y)
        } else {
            (g.frame, g.value)
        };
        let (c2x, c2y) = if a.back.enabled {
            (a.frame + a.back.x * half, a.value + a.back.y)
        } else {
            (a.frame, a.value)
        };
        Some(bezier_at(
            frame,
            [g.frame, g.value],
            [c1x, c1y],
            [c2x, c2y],
            [a.frame, a.value],
        ))
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
}

fn cubic(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

fn bezier_at(x: f32, p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2]) -> f32 {
    let span = p3[0] - p0[0];
    if span <= 0.0 {
        return p3[1];
    }
    let mut t = ((x - p0[0]) / span).clamp(0.0, 1.0);
    for _ in 0..8 {
        let fx = cubic(p0[0], p1[0], p2[0], p3[0], t) - x;
        let u = 1.0 - t;
        let dx =
            3.0 * u * u * (p1[0] - p0[0]) + 6.0 * u * t * (p2[0] - p1[0]) + 3.0 * t * t * (p3[0] - p2[0]);
        if dx.abs() < 1e-6 {
            break;
        }
        let next = (t - fx / dx).clamp(0.0, 1.0);
        if (next - t).abs() < 1e-6 {
            t = next;
            break;
        }
        t = next;
    }
    if (cubic(p0[0], p1[0], p2[0], p3[0], t) - x).abs() > 1e-3 * span.max(1.0) {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if cubic(p0[0], p1[0], p2[0], p3[0], mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        t = 0.5 * (lo + hi);
    }
    cubic(p0[1], p1[1], p2[1], p3[1], t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prop(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    fn key(frame: f32, value: f32) -> Value {
        json!({
            "frame": frame, "value": value,
            "front": {"enabled": true, "x": 1, "y": 0},
            "back": {"enabled": true, "x": -1, "y": 0},
        })
    }

    #[test]
    fn parses_options_and_channels() {
        let a = PropertyAnimation::parse(&prop(json!({
            "value": "1 2 0",
            "relative": true,
            "animation": {
                "c0": [key(0.0, 0.0), key(30.0, 10.0)],
                "c1": [key(0.0, 0.0), key(30.0, -5.0)],
                "options": {"fps": 15, "length": 60, "mode": "loop", "wraploop": true,
                            "startpaused": true, "name": "n", "parent": {"key": "alpha"},
                            "children": [{"key": "scale"}], "events": [{"frame": 30, "name": "e"}]}
            }
        })))
        .unwrap();
        assert_eq!(a.channel_count(), 2);
        assert_eq!(a.fps, 15.0);
        assert_eq!(a.length, 60.0);
        assert_eq!(a.mode, PlayMode::Loop);
        assert!(a.wraploop && a.startpaused && a.relative);
        assert_eq!(a.name.as_deref(), Some("n"));
        assert_eq!(a.parent.as_deref(), Some("alpha"));
        assert_eq!(a.children, vec!["scale".to_owned()]);
        assert_eq!(a.events[0].name, "e");
    }

    #[test]
    fn defaults_when_options_missing() {
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 1.0,
            "animation": {"c0": [key(0.0, 1.0), key(20.0, 0.0)]}
        })))
        .unwrap();
        assert_eq!(a.fps, 30.0);
        assert_eq!(a.length, 20.0);
        assert_eq!(a.mode, PlayMode::Single);
        assert!(!a.wraploop && !a.startpaused && !a.relative);
    }

    #[test]
    fn no_animation_is_none() {
        assert!(PropertyAnimation::parse(&prop(json!({"value": 1.0}))).is_none());
        assert!(
            PropertyAnimation::parse(&prop(json!({"value": 1.0, "animation": {"options": {}}}))).is_none()
        );
    }

    #[test]
    fn eased_segment_hits_endpoints_and_midpoint() {
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 0.0,
            "animation": {"c0": [key(0.0, 0.0), key(30.0, 10.0)], "options": {"length": 30}}
        })))
        .unwrap();
        assert_eq!(a.sample(0, -5.0), Some(0.0));
        assert_eq!(a.sample(0, 0.0), Some(0.0));
        assert!((a.sample(0, 15.0).unwrap() - 5.0).abs() < 1e-3);
        assert!((a.sample(0, 30.0).unwrap() - 10.0).abs() < 1e-5);
        assert_eq!(a.sample(0, 40.0), Some(10.0));
        let q = a.sample(0, 7.5).unwrap();
        assert!(q > 0.0 && q < 2.5, "eased start should lag linear: {q}");
    }

    #[test]
    fn linear_when_handles_disabled() {
        let k = |f: f32, v: f32| json!({"frame": f, "value": v, "front": {"enabled": false}, "back": {"enabled": false}});
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 0.0,
            "animation": {"c0": [k(0.0, 0.0), k(10.0, 10.0)]}
        })))
        .unwrap();
        for f in [1.0, 2.5, 6.0, 9.0] {
            assert!((a.sample(0, f).unwrap() - f).abs() < 1e-4);
        }
    }

    #[test]
    fn step_holds_previous_value() {
        let mut k1 = key(10.0, 5.0);
        k1["step"] = json!(true);
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 0.0,
            "animation": {"c0": [key(0.0, 1.0), k1]}
        })))
        .unwrap();
        assert_eq!(a.sample(0, 9.99), Some(1.0));
        assert_eq!(a.sample(0, 10.0), Some(5.0));
    }

    #[test]
    fn wraploop_returns_to_first_value_at_length() {
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 0.0,
            "animation": {"c0": [key(0.0, 2.0), key(20.0, 8.0)],
                          "options": {"length": 40, "mode": "loop", "wraploop": true}}
        })))
        .unwrap();
        assert!((a.sample(0, 20.0).unwrap() - 8.0).abs() < 1e-5);
        assert!((a.sample(0, 30.0).unwrap() - 5.0).abs() < 1e-3);
        assert!((a.sample(0, 40.0).unwrap() - 2.0).abs() < 1e-5);
    }

    #[test]
    fn without_wraploop_holds_last_value() {
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 0.0,
            "animation": {"c0": [key(0.0, 2.0), key(20.0, 8.0)],
                          "options": {"length": 40, "mode": "loop"}}
        })))
        .unwrap();
        assert_eq!(a.sample(0, 30.0), Some(8.0));
    }

    #[test]
    fn missing_channel_is_none() {
        let a = PropertyAnimation::parse(&prop(json!({
            "value": 0.0,
            "animation": {"c0": [key(0.0, 2.0), key(20.0, 8.0)]}
        })))
        .unwrap();
        assert_eq!(a.sample(1, 5.0), None);
    }
}
