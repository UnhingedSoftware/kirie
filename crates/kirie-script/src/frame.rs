use std::collections::BTreeMap;

use serde::Serialize;

use crate::value::ScriptValue;

#[derive(Debug, Default, Serialize)]
pub struct LayerState {
    pub id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub angles: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(rename = "parallaxDepth", skip_serializing_if = "Option::is_none")]
    pub parallax_depth: Option<f32>,
    #[serde(rename = "pointSize", skip_serializing_if = "Option::is_none")]
    pub point_size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<(String, u32)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solid: Option<bool>,
}

impl Clone for LayerState {
    fn clone(&self) -> Self {
        LayerState {
            id: self.id,
            name: self.name.clone(),
            parent: self.parent,
            origin: self.origin,
            scale: self.scale,
            angles: self.angles,
            color: self.color,
            alpha: self.alpha,
            visible: self.visible,
            parallax_depth: self.parallax_depth,
            point_size: self.point_size,
            text: self.text.clone(),
            size: self.size,
            solid: self.solid,
            effects: self.effects.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        let LayerState {
            id,
            name,
            parent,
            origin,
            scale,
            angles,
            color,
            alpha,
            visible,
            parallax_depth,
            point_size,
            text,
            size,
            solid,
            effects,
        } = source;
        self.id = *id;
        self.name.clone_from(name);
        self.parent = *parent;
        self.origin = *origin;
        self.scale = *scale;
        self.angles = *angles;
        self.color = *color;
        self.alpha = *alpha;
        self.visible = *visible;
        self.parallax_depth = *parallax_depth;
        self.point_size = *point_size;
        self.text.clone_from(text);
        self.size = *size;
        self.solid = *solid;
        self.effects.clone_from(effects);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CameraState {
    pub eye: [f32; 3],
    pub center: [f32; 3],
    pub up: [f32; 3],
    pub fov: f32,
}

impl Default for CameraState {
    fn default() -> Self {
        CameraState {
            eye: [0.0, 0.0, 0.0],
            center: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            fov: 45.0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SceneState {
    pub bloom: bool,
    pub bloomstrength: i64,
    pub bloomthreshold: i64,
    pub clearenabled: bool,
    pub clearcolor: [f32; 3],
    pub ambientcolor: [f32; 3],
    pub skylightcolor: [f32; 3],
    pub fov: f32,
    pub nearz: f32,
    pub farz: f32,
    pub camerafade: bool,
    pub camerashake: bool,
    pub camerashakespeed: f32,
    pub camerashakeamplitude: f32,
    pub camerashakeroughness: f32,
    pub cameraparallax: bool,
    pub cameraparallaxamount: f32,
    pub cameraparallaxdelay: f32,
    pub cameraparallaxmouseinfluence: f32,
    pub camera: CameraState,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AudioBuffers {
    #[serde(rename = "a16")]
    pub audio16: Vec<f32>,
    #[serde(rename = "a32")]
    pub audio32: Vec<f32>,
    #[serde(rename = "a64")]
    pub audio64: Vec<f32>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AnimationState {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub fps: f32,
    pub frames: f32,
    pub duration: f32,
    pub rate: f32,
    pub playing: bool,
    pub frame: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostFrame {
    pub runtime: f64,
    pub frametime: f64,
    #[serde(rename = "timeOfDay")]
    pub time_of_day: f64,
    pub now: f64,
    #[serde(rename = "resX")]
    pub res_x: f64,
    #[serde(rename = "resY")]
    pub res_y: f64,
    #[serde(rename = "userProps")]
    pub user_props: BTreeMap<String, ScriptValue>,
    #[serde(rename = "pointerScreen")]
    pub pointer_screen: [f32; 2],
    #[serde(rename = "pointerWorld")]
    pub pointer_world: [f32; 3],
    #[serde(rename = "pointerLeftDown")]
    pub pointer_left_down: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioBuffers>,
    pub scene: SceneState,
    pub layers: Vec<LayerState>,
    #[serde(rename = "workshopId", skip_serializing_if = "Option::is_none")]
    pub workshop_id: Option<String>,
    #[serde(skip)]
    pub media: Option<MediaFrame>,
    pub animations: Vec<AnimationState>,
    #[serde(skip)]
    pub animation_events: Vec<(i64, String, f32)>,
}

#[derive(Clone, Debug, Default)]
pub struct MediaFrame {
    pub enabled: bool,
    pub state: i32,
    pub title: String,
    pub artist: String,
    pub album_title: String,
    pub position: f64,
    pub duration: f64,
    pub has_thumbnail: bool,
    pub colors: Option<[[f32; 3]; 5]>,
    pub status_changed: bool,
    pub playback_changed: bool,
    pub properties_changed: bool,
    pub thumbnail_changed: bool,
    pub timeline_changed: bool,
}

impl Default for HostFrame {
    fn default() -> Self {
        HostFrame {
            runtime: 0.0,
            frametime: 0.0,
            time_of_day: 0.0,
            now: 0.0,
            res_x: 1920.0,
            res_y: 1080.0,
            user_props: BTreeMap::new(),
            pointer_screen: [0.0, 0.0],
            pointer_world: [0.0, 0.0, 0.0],
            pointer_left_down: false,
            audio: None,
            scene: SceneState::default(),
            layers: Vec::new(),
            workshop_id: None,
            media: None,
            animations: Vec::new(),
            animation_events: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SceneOp {
    SetProperty {
        layer_id: i64,
        name: String,
        value: ScriptValue,
    },
    SetParent {
        layer_id: i64,
        parent: Option<i64>,
    },
    SetCameraTransforms {
        eye: Option<[f32; 3]>,
        center: Option<[f32; 3]>,
        up: Option<[f32; 3]>,
        fov: Option<f32>,
        zoom: Option<f32>,
    },
    CreateLayer {
        layer_id: i64,
        path: String,
        workshop_id: Option<String>,
        text: Option<String>,
    },
    SortLayer {
        layer_id: i64,
        index: i64,
    },
    DestroyLayer {
        layer_id: i64,
    },
    ParticleCommand {
        layer_id: i64,
        cmd: String,
    },
    EmitParticles {
        layer_id: i64,
        count: u32,
    },
    SetMaterialProperty {
        layer_id: i64,
        effect: u32,
        name: String,
        value: ScriptValue,
    },
    VideoCommand {
        layer_id: i64,
        cmd: String,
        value: f64,
    },
    SetInstance {
        layer_id: i64,
        name: String,
        value: ScriptValue,
    },
    SetSceneProperty {
        name: String,
        value: ScriptValue,
    },
    AnimationCommand {
        index: u32,
        cmd: String,
        value: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    pub error: bool,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct TickOutput {
    pub property_results: Vec<(String, ScriptValue)>,
    pub ops: Vec<SceneOp>,
    pub logs: Vec<LogLine>,
    pub errors: Vec<crate::ScriptError>,
}
