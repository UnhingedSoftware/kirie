use kirie_scene::object::{Effect, ObjectKind};
use kirie_scene::value::DynamicValue;
use kirie_scene::{PlayMode, PropertyAnimation, SceneModel, UserSetting};
use kirie_script::ScriptValue;

use super::scripting::{PropTarget, PropUpdate};

pub const SCENE_OBJECT: i64 = -1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnimTarget {
    Layer(PropTarget),
    EffectConstant { effect: usize, name: String },
    ParticleOverride(String),
    SceneZoom,
    TextMaxWidth,
}

pub struct AnimatedProp {
    pub object_id: i64,
    pub key: String,
    pub target: AnimTarget,
    base: Vec<f32>,
    anim: PropertyAnimation,
    pos: f32,
    playing: bool,
    finished: bool,
    rate: f32,
    last: Option<Vec<f32>>,
    parent: Option<usize>,
}

impl AnimatedProp {
    fn frame(&self) -> f32 {
        let len = self.anim.length.max(0.0);
        match self.anim.mode {
            PlayMode::Single | PlayMode::TimeOfDay => self.pos.clamp(0.0, len),
            PlayMode::Loop => {
                if len > 0.0 {
                    self.pos.rem_euclid(len)
                } else {
                    0.0
                }
            }
            PlayMode::Mirror => {
                if len > 0.0 {
                    let p = self.pos.rem_euclid(2.0 * len);
                    if p > len { 2.0 * len - p } else { p }
                } else {
                    0.0
                }
            }
        }
    }

    fn current(&self) -> Vec<f32> {
        let frame = self.frame();
        let mut out = self.base.clone();
        for (ch, slot) in out.iter_mut().enumerate() {
            if let Some(s) = self.anim.sample(ch, frame) {
                *slot = if self.anim.relative { self.base[ch] + s } else { s };
            }
        }
        out
    }

    fn advance(&mut self, dt: f32, time_of_day: f32, events: &mut Vec<(i64, String, f32)>) {
        if !self.playing {
            return;
        }
        let prev = self.frame();
        let len = self.anim.length;
        match self.anim.mode {
            PlayMode::TimeOfDay => self.pos = time_of_day * len,
            PlayMode::Single => {
                self.pos += dt * self.anim.fps * self.rate;
                if self.pos >= len {
                    self.pos = len;
                    self.playing = false;
                    self.finished = true;
                }
            }
            PlayMode::Loop | PlayMode::Mirror => self.pos += dt * self.anim.fps * self.rate,
        }
        let now = self.frame();
        for e in &self.anim.events {
            let crossed = if now >= prev {
                prev < e.frame && e.frame <= now
            } else {
                e.frame > prev || e.frame <= now
            };
            if crossed {
                events.push((self.object_id, e.name.clone(), e.frame));
            }
        }
    }

    fn value(&self, v: &[f32]) -> ScriptValue {
        match (&self.target, v) {
            (AnimTarget::Layer(PropTarget::Color), [r, g, b, ..]) => ScriptValue::Vec3([*r, *g, *b]),
            (
                AnimTarget::Layer(PropTarget::Origin | PropTarget::Scale | PropTarget::Angles),
                [x, y, z, ..],
            ) => ScriptValue::Vec3([*x, *y, *z]),
            (_, [a, b, c, d, ..]) => ScriptValue::Vec4([*a, *b, *c, *d]),
            (_, [a, b, c]) => ScriptValue::Vec3([*a, *b, *c]),
            (_, [a, b]) => ScriptValue::Vec2([*a, *b]),
            (_, [a]) => ScriptValue::Float(f64::from(*a)),
            _ => ScriptValue::Null,
        }
    }
}

#[derive(Default)]
pub struct AnimOutput {
    pub updates: Vec<PropUpdate>,
    pub overrides: Vec<(String, ScriptValue)>,
    pub effect: Vec<(i64, usize, String, ScriptValue)>,
    pub particle: Vec<(i64, String, f32)>,
    pub zoom: Option<f32>,
    pub text_width: Vec<(i64, f32)>,
    pub events: Vec<(i64, String, f32)>,
}

pub struct PropertyAnimator {
    tracks: Vec<AnimatedProp>,
}

impl PropertyAnimator {
    #[must_use]
    pub fn build(model: &SceneModel) -> Option<Self> {
        let mut tracks: Vec<AnimatedProp> = Vec::new();
        push(
            &mut tracks,
            SCENE_OBJECT,
            "zoom",
            AnimTarget::SceneZoom,
            &model.scene.general.zoom,
            |v| vec![*v],
        );
        for object in &model.scene.objects {
            let id = object.base.id;
            let base = &object.base;
            push(
                &mut tracks,
                id,
                "origin",
                AnimTarget::Layer(PropTarget::Origin),
                &base.origin,
                |v| v.to_vec(),
            );
            push(
                &mut tracks,
                id,
                "scale",
                AnimTarget::Layer(PropTarget::Scale),
                &base.scale,
                |v| v.to_vec(),
            );
            push(
                &mut tracks,
                id,
                "angles",
                AnimTarget::Layer(PropTarget::Angles),
                &base.angles,
                |v| v.to_vec(),
            );
            match &object.kind {
                ObjectKind::Image(img) => {
                    push(
                        &mut tracks,
                        id,
                        "alpha",
                        AnimTarget::Layer(PropTarget::Alpha),
                        &img.alpha,
                        |v| vec![*v],
                    );
                    push(
                        &mut tracks,
                        id,
                        "brightness",
                        AnimTarget::Layer(PropTarget::Brightness),
                        &img.brightness,
                        |v| vec![*v],
                    );
                    push(
                        &mut tracks,
                        id,
                        "color",
                        AnimTarget::Layer(PropTarget::Color),
                        &img.color,
                        |v| v[..3].to_vec(),
                    );
                    push_effects(&mut tracks, id, &img.effects);
                }
                ObjectKind::Text(txt) => {
                    push(
                        &mut tracks,
                        id,
                        "alpha",
                        AnimTarget::Layer(PropTarget::Alpha),
                        &txt.alpha,
                        |v| vec![*v],
                    );
                    push(
                        &mut tracks,
                        id,
                        "color",
                        AnimTarget::Layer(PropTarget::Color),
                        &txt.color,
                        |v| v[..3].to_vec(),
                    );
                    push(
                        &mut tracks,
                        id,
                        "maxwidth",
                        AnimTarget::TextMaxWidth,
                        &txt.maxwidth,
                        |v| vec![*v],
                    );
                    push_effects(&mut tracks, id, &txt.effects);
                }
                ObjectKind::Particle(pobj) => {
                    let io = &pobj.instanceoverride;
                    for (name, us) in [
                        ("alpha", &io.alpha),
                        ("size", &io.size),
                        ("lifetime", &io.lifetime),
                        ("rate", &io.rate),
                        ("speed", &io.speed),
                        ("count", &io.count),
                    ] {
                        push(
                            &mut tracks,
                            id,
                            name,
                            AnimTarget::ParticleOverride(name.to_owned()),
                            us,
                            |v| vec![*v],
                        );
                    }
                }
                _ => {}
            }
        }
        if tracks.is_empty() {
            return None;
        }
        let parents: Vec<Option<usize>> = tracks
            .iter()
            .map(|t| {
                let pkey = t.anim.parent.as_deref()?;
                tracks
                    .iter()
                    .position(|p| p.object_id == t.object_id && p.key == pkey && p.anim.parent.is_none())
            })
            .collect();
        for (t, p) in tracks.iter_mut().zip(parents) {
            t.parent = p;
        }
        for i in 0..tracks.len() {
            if let Some(p) = tracks[i].parent {
                let playing = tracks[p].playing;
                tracks[i].playing = playing;
            }
        }
        tracing::debug!(count = tracks.len(), "property animations loaded");
        Some(PropertyAnimator { tracks })
    }

    pub fn tick(&mut self, dt: f32, time_of_day: f32) -> AnimOutput {
        let mut out = AnimOutput::default();
        for i in 0..self.tracks.len() {
            if self.tracks[i].parent.is_none() {
                self.tracks[i].advance(dt, time_of_day, &mut out.events);
            }
        }
        for i in 0..self.tracks.len() {
            if let Some(p) = self.tracks[i].parent {
                let (pos, playing, finished) = {
                    let par = &self.tracks[p];
                    (par.pos, par.playing, par.finished)
                };
                let t = &mut self.tracks[i];
                t.pos = pos;
                t.playing = playing;
                t.finished = finished;
            }
        }
        for t in &mut self.tracks {
            let cur = t.current();
            if t.last.as_ref() == Some(&cur) {
                continue;
            }
            let value = t.value(&cur);
            t.last = Some(cur.clone());
            match &t.target {
                AnimTarget::Layer(target) => {
                    out.overrides
                        .push((format!("{}_{}", t.key, t.object_id), value.clone()));
                    out.updates.push(PropUpdate {
                        object_id: t.object_id,
                        target: *target,
                        value,
                    });
                }
                AnimTarget::EffectConstant { effect, name } => {
                    out.overrides
                        .push((format!("fx{effect}{name}_{}", t.object_id), value.clone()));
                    out.effect.push((t.object_id, *effect, name.clone(), value));
                }
                AnimTarget::ParticleOverride(name) => {
                    out.particle.push((t.object_id, name.clone(), cur[0]));
                }
                AnimTarget::SceneZoom => out.zoom = Some(cur[0]),
                AnimTarget::TextMaxWidth => out.text_width.push((t.object_id, cur[0])),
            }
        }
        out
    }

    #[must_use]
    pub fn find(&self, object_id: i64, name: Option<&str>) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.object_id == object_id && t.parent.is_none())
            .filter(|(_, t)| name.is_none_or(|n| t.anim.name.as_deref() == Some(n)))
            .map(|(i, _)| i)
            .collect()
    }

    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.parent.is_none() && t.anim.name.as_deref() == Some(name))
            .map(|(i, _)| i)
            .collect()
    }

    fn root(&self, idx: usize) -> usize {
        let mut i = idx;
        while let Some(p) = self.tracks.get(i).and_then(|t| t.parent) {
            i = p;
        }
        i
    }

    pub fn play(&mut self, idx: usize) {
        let r = self.root(idx);
        if let Some(t) = self.tracks.get_mut(r) {
            if t.finished {
                t.pos = 0.0;
                t.finished = false;
            }
            t.playing = true;
        }
    }

    pub fn pause(&mut self, idx: usize) {
        let r = self.root(idx);
        if let Some(t) = self.tracks.get_mut(r) {
            t.playing = false;
        }
    }

    pub fn stop(&mut self, idx: usize) {
        let r = self.root(idx);
        if let Some(t) = self.tracks.get_mut(r) {
            t.playing = false;
            t.finished = false;
            t.pos = 0.0;
        }
    }

    pub fn set_frame(&mut self, idx: usize, frame: f32) {
        let r = self.root(idx);
        if let Some(t) = self.tracks.get_mut(r) {
            t.pos = frame.max(0.0);
            t.finished = false;
        }
    }

    pub fn set_rate(&mut self, idx: usize, rate: f32) {
        let r = self.root(idx);
        if let Some(t) = self.tracks.get_mut(r) {
            t.rate = rate;
        }
    }

    #[must_use]
    pub fn is_playing(&self, idx: usize) -> bool {
        self.tracks.get(self.root(idx)).is_some_and(|t| t.playing)
    }

    #[must_use]
    pub fn frame(&self, idx: usize) -> f32 {
        self.tracks.get(self.root(idx)).map_or(0.0, AnimatedProp::frame)
    }

    #[must_use]
    pub fn info(&self, idx: usize) -> Option<(f32, f32, f32, Option<&str>)> {
        let t = self.tracks.get(self.root(idx))?;
        Some((t.anim.fps, t.anim.length, t.rate, t.anim.name.as_deref()))
    }
}

fn push<T>(
    tracks: &mut Vec<AnimatedProp>,
    object_id: i64,
    key: &str,
    target: AnimTarget,
    setting: &UserSetting<T>,
    base: impl FnOnce(&T) -> Vec<f32>,
) {
    let Some(anim) = &setting.animation else { return };
    let base = base(&setting.value);
    if base.is_empty() {
        return;
    }
    tracks.push(AnimatedProp {
        object_id,
        key: key.to_owned(),
        target,
        base,
        playing: !anim.startpaused,
        anim: anim.clone(),
        pos: 0.0,
        finished: false,
        rate: 1.0,
        last: None,
        parent: None,
    });
}

fn push_effects(tracks: &mut Vec<AnimatedProp>, id: i64, effects: &[Effect]) {
    for (ei, eff) in effects.iter().enumerate() {
        for pass in &eff.passes {
            for (name, us) in &pass.constantshadervalues {
                push(
                    tracks,
                    id,
                    name,
                    AnimTarget::EffectConstant {
                        effect: ei,
                        name: name.clone(),
                    },
                    us,
                    |v| match v {
                        DynamicValue::Float(f) => vec![*f as f32],
                        DynamicValue::Int(i) => vec![*i as f32],
                        DynamicValue::Bool(b) => vec![f32::from(u8::from(*b))],
                        DynamicValue::Vec(v) => v.clone(),
                        DynamicValue::Color(c) => c.to_vec(),
                        DynamicValue::Str(_) | DynamicValue::Null => Vec::new(),
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn anim(v: serde_json::Value) -> PropertyAnimation {
        PropertyAnimation::parse(v.as_object().unwrap()).unwrap()
    }

    fn key(frame: f32, value: f32) -> serde_json::Value {
        json!({"frame": frame, "value": value,
               "front": {"enabled": false}, "back": {"enabled": false}})
    }

    fn track(a: PropertyAnimation, base: Vec<f32>, target: AnimTarget) -> AnimatedProp {
        AnimatedProp {
            object_id: 7,
            key: "alpha".into(),
            target,
            base,
            playing: !a.startpaused,
            anim: a,
            pos: 0.0,
            finished: false,
            rate: 1.0,
            last: None,
            parent: None,
        }
    }

    #[test]
    fn single_plays_once_and_holds() {
        let a = anim(
            json!({"value": 1.0, "animation": {"c0": [key(0.0, 1.0), key(10.0, 0.0)],
            "options": {"fps": 10, "length": 10, "mode": "single"}}}),
        );
        let mut an = PropertyAnimator {
            tracks: vec![track(a, vec![1.0], AnimTarget::Layer(PropTarget::Alpha))],
        };
        let first = an.tick(0.0, 0.0);
        assert_eq!(first.updates.len(), 1);
        assert_eq!(first.updates[0].value, ScriptValue::Float(1.0));
        assert_eq!(first.overrides[0].0, "alpha_7");
        let mid = an.tick(0.5, 0.0);
        assert!(matches!(mid.updates[0].value, ScriptValue::Float(f) if (f - 0.5).abs() < 1e-4));
        an.tick(1.0, 0.0);
        assert!(!an.is_playing(0));
        assert_eq!(an.frame(0), 10.0);
        assert!(an.tick(1.0, 0.0).updates.is_empty());
        an.play(0);
        assert!(an.is_playing(0));
        assert_eq!(an.frame(0), 0.0);
    }

    #[test]
    fn loop_wraps_and_relative_adds_offset() {
        let a = anim(json!({"value": "1 2 0", "relative": true, "animation": {
            "c0": [key(0.0, 0.0), key(4.0, 8.0)],
            "options": {"fps": 1, "length": 4, "mode": "loop"}}}));
        let mut an = PropertyAnimator {
            tracks: vec![track(
                a,
                vec![1.0, 2.0, 0.0],
                AnimTarget::Layer(PropTarget::Origin),
            )],
        };
        an.tick(0.0, 0.0);
        let o = an.tick(1.0, 0.0);
        assert_eq!(o.updates[0].value, ScriptValue::Vec3([3.0, 2.0, 0.0]));
        assert!(an.tick(4.0, 0.0).updates.is_empty());
        let o = an.tick(1.0, 0.0);
        assert_eq!(o.updates[0].value, ScriptValue::Vec3([5.0, 2.0, 0.0]));
    }

    #[test]
    fn mirror_ping_pongs() {
        let a = anim(
            json!({"value": 0.0, "animation": {"c0": [key(0.0, 0.0), key(4.0, 4.0)],
            "options": {"fps": 1, "length": 4, "mode": "mirror"}}}),
        );
        let mut an = PropertyAnimator {
            tracks: vec![track(a, vec![0.0], AnimTarget::SceneZoom)],
        };
        an.tick(0.0, 0.0);
        assert_eq!(an.tick(3.0, 0.0).zoom, Some(3.0));
        assert_eq!(an.tick(2.0, 0.0).zoom, None);
        assert_eq!(an.tick(1.0, 0.0).zoom, Some(2.0));
        assert_eq!(an.tick(3.0, 0.0).zoom, Some(1.0));
    }

    #[test]
    fn paused_start_still_applies_frame_zero_and_events_fire() {
        let a = anim(
            json!({"value": 1.0, "animation": {"c0": [key(0.0, 0.0), key(10.0, 1.0)],
            "options": {"fps": 10, "length": 10, "mode": "single", "startpaused": true,
                        "events": [{"frame": 5, "name": "half"}]}}}),
        );
        let mut an = PropertyAnimator {
            tracks: vec![track(a, vec![1.0], AnimTarget::ParticleOverride("alpha".into()))],
        };
        let o = an.tick(0.5, 0.0);
        assert_eq!(o.particle, vec![(7, "alpha".to_owned(), 0.0)]);
        assert!(!an.is_playing(0));
        an.play(0);
        let o = an.tick(0.6, 0.0);
        assert_eq!(o.events, vec![(7, "half".to_owned(), 5.0)]);
        an.stop(0);
        assert_eq!(an.frame(0), 0.0);
        assert!(!an.is_playing(0));
    }

    #[test]
    fn children_follow_parent() {
        let parent = anim(
            json!({"value": 0.0, "animation": {"c0": [key(0.0, 0.0), key(10.0, 10.0)],
            "options": {"fps": 10, "length": 10, "mode": "single", "startpaused": true, "name": "n",
                        "children": [{"key": "b"}]}}}),
        );
        let child = anim(
            json!({"value": 0.0, "animation": {"c0": [key(0.0, 0.0), key(10.0, 20.0)],
            "options": {"fps": 10, "length": 10, "mode": "single", "parent": {"key": "alpha"}}}}),
        );
        let mut c = track(child, vec![0.0], AnimTarget::TextMaxWidth);
        c.key = "b".into();
        c.playing = true;
        c.parent = Some(0);
        let mut an = PropertyAnimator {
            tracks: vec![track(parent, vec![0.0], AnimTarget::SceneZoom), c],
        };
        let o = an.tick(0.5, 0.0);
        assert_eq!(o.zoom, Some(0.0));
        assert_eq!(o.text_width, vec![(7, 0.0)]);
        assert_eq!(an.find(7, Some("n")), vec![0]);
        assert_eq!(an.find(7, None), vec![0]);
        an.play(1);
        let o = an.tick(0.5, 0.0);
        assert_eq!(o.zoom, Some(5.0));
        assert_eq!(o.text_width, vec![(7, 10.0)]);
    }
}
