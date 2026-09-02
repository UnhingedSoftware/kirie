use serde::{Deserialize, Serialize};

use crate::material::{EffectFile, Material, ModelFile};
use crate::object::{Effect, ImageObject, Object, ObjectKind, ParticleObject};
use crate::particle::ParticleSystem;
use crate::property::{PropertyBag, Resolvable};
use crate::scene::{Camera, General, Scene};
use crate::user::{ConstantValues, UserRef, UserSetting};

pub fn resolve_us<T: Resolvable + Clone>(us: &mut UserSetting<T>, bag: &PropertyBag) {
    if let Some(sb) = &mut us.script {
        resolve_script_properties(&mut sb.properties, bag);
    }
    match &us.user {
        Some(UserRef::Name(name)) => {
            if let Some(v) = bag.get(name) {
                us.value = T::from_property(v);
            }
        }
        Some(UserRef::Conditional { name, condition }) => {
            let matches = bag
                .get(name)
                .is_some_and(|v| &v.as_condition_string() == condition);
            us.value = T::from_bool(matches);
        }
        None => {}
    }
}

pub fn resolve_script_properties(props: &mut serde_json::Map<String, serde_json::Value>, bag: &PropertyBag) {
    use serde_json::Value;
    for v in props.values_mut() {
        let Value::Object(o) = v else { continue };
        let Some(fallback) = o.get("value").cloned() else {
            continue;
        };
        let resolved = match o.get("user") {
            Some(Value::String(name)) => bag.get(name).map_or(fallback, |pv| match pv {
                crate::property::PropertyValue::Bool(b) => Value::Bool(*b),
                crate::property::PropertyValue::Number(n) => {
                    serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number)
                }
                other => Value::String(other.as_condition_string()),
            }),
            Some(Value::Object(u)) => {
                let name = u.get("name").and_then(Value::as_str);
                let condition = u.get("condition").and_then(Value::as_str);
                match (name, condition) {
                    (Some(name), Some(condition)) => match bag.get(name) {
                        Some(pv) => Value::Bool(pv.as_condition_string() == condition),
                        None => fallback,
                    },
                    _ => fallback,
                }
            }
            _ => fallback,
        };
        *v = resolved;
    }
}

pub fn resolve_constants(constants: &mut ConstantValues, bag: &PropertyBag) {
    for us in constants.values_mut() {
        resolve_us(us, bag);
    }
}

impl Camera {
    pub fn reresolve(&mut self, bag: &PropertyBag) {
        resolve_us(&mut self.fov, bag);
    }
}

impl General {
    pub fn resolve(&mut self, bag: &PropertyBag) {
        resolve_us(&mut self.ambientcolor, bag);
        resolve_us(&mut self.skylightcolor, bag);
        resolve_us(&mut self.clearcolor, bag);
        resolve_us(&mut self.camerafade, bag);
        resolve_us(&mut self.bloom, bag);
        resolve_us(&mut self.bloomstrength, bag);
        resolve_us(&mut self.bloomthreshold, bag);
        resolve_us(&mut self.cameraparallax, bag);
        resolve_us(&mut self.cameraparallaxamount, bag);
        resolve_us(&mut self.cameraparallaxdelay, bag);
        resolve_us(&mut self.cameraparallaxmouseinfluence, bag);
        resolve_us(&mut self.camerashake, bag);
        resolve_us(&mut self.camerashakeamplitude, bag);
        resolve_us(&mut self.camerashakeroughness, bag);
        resolve_us(&mut self.camerashakespeed, bag);
    }
}

impl Object {
    fn resolve(&mut self, bag: &PropertyBag) {
        resolve_us(&mut self.base.origin, bag);
        resolve_us(&mut self.base.scale, bag);
        resolve_us(&mut self.base.angles, bag);
        resolve_us(&mut self.base.visible, bag);
        match &mut self.kind {
            ObjectKind::Image(img) => img.resolve(bag),
            ObjectKind::Particle(p) => p.resolve(bag),
            ObjectKind::Text(t) => {
                resolve_us(&mut t.text, bag);
                resolve_us(&mut t.pointsize, bag);
                resolve_us(&mut t.scale, bag);
                resolve_us(&mut t.color, bag);
                resolve_us(&mut t.alpha, bag);
                resolve_us(&mut t.visible, bag);
            }
            ObjectKind::Sound(_)
            | ObjectKind::Model(_)
            | ObjectKind::Light(_)
            | ObjectKind::Shape(_)
            | ObjectKind::Group => {}
        }
    }
}

impl ImageObject {
    fn resolve(&mut self, bag: &PropertyBag) {
        resolve_us(&mut self.scale, bag);
        resolve_us(&mut self.angles, bag);
        resolve_us(&mut self.visible, bag);
        resolve_us(&mut self.alpha, bag);
        resolve_us(&mut self.color, bag);
        resolve_us(&mut self.parallax_depth, bag);
        resolve_us(&mut self.color_blend_mode, bag);
        resolve_us(&mut self.brightness, bag);
        if let Some(material) = &mut self.material {
            resolve_material(material, bag);
        }
        for effect in &mut self.effects {
            effect.resolve(bag);
        }
        for layer in &mut self.animationlayers {
            resolve_us(&mut layer.rate, bag);
            resolve_us(&mut layer.visible, bag);
            resolve_us(&mut layer.blend, bag);
            resolve_us(&mut layer.animation, bag);
        }
    }
}

impl Effect {
    fn resolve(&mut self, bag: &PropertyBag) {
        resolve_us(&mut self.visible, bag);
        for pass in &mut self.passes {
            resolve_constants(&mut pass.constantshadervalues, bag);
        }
        if let Some(file) = &mut self.resolved {
            for pass in &mut file.passes {
                if let Some(material) = &mut pass.resolved {
                    resolve_material(material, bag);
                }
            }
        }
    }
}

impl ParticleObject {
    fn resolve(&mut self, bag: &PropertyBag) {
        resolve_us(&mut self.scale, bag);
        resolve_us(&mut self.angles, bag);
        resolve_us(&mut self.visible, bag);
        resolve_us(&mut self.parallax_depth, bag);
        let ov = &mut self.instanceoverride;
        resolve_us(&mut ov.enabled, bag);
        resolve_us(&mut ov.alpha, bag);
        resolve_us(&mut ov.size, bag);
        resolve_us(&mut ov.lifetime, bag);
        resolve_us(&mut ov.rate, bag);
        resolve_us(&mut ov.speed, bag);
        resolve_us(&mut ov.count, bag);
        resolve_us(&mut ov.color, bag);
        resolve_us(&mut ov.colorn, bag);
        for stage in self
            .system
            .initializers
            .iter_mut()
            .chain(&mut self.system.operators)
        {
            resolve_constants(&mut stage.params, bag);
        }
        if let Some(material) = &mut self.system.resolved_material {
            resolve_material(material, bag);
        }
    }
}

fn resolve_material(material: &mut Material, bag: &PropertyBag) {
    for pass in &mut material.passes {
        resolve_constants(&mut pass.constantshadervalues, bag);
    }
}

pub trait AssetSource: Sync {
    fn load(&self, path: &str) -> Option<Vec<u8>>;
}

impl<F: Fn(&str) -> Option<Vec<u8>> + Sync> AssetSource for F {
    fn load(&self, path: &str) -> Option<Vec<u8>> {
        self(path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetProblem {
    pub path: String,
    pub reason: String,
}

fn load_json(
    source: &dyn AssetSource,
    path: &str,
    problems: &mut Vec<AssetProblem>,
) -> Option<serde_json::Value> {
    let Some(bytes) = source.load(path) else {
        problems.push(AssetProblem {
            path: path.to_owned(),
            reason: "asset not found".to_owned(),
        });
        return None;
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            problems.push(AssetProblem {
                path: path.to_owned(),
                reason: format!("invalid JSON: {e}"),
            });
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneModel {
    pub scene: Scene,
}

impl SceneModel {
    pub fn resolve(mut scene: Scene, bag: &PropertyBag) -> Self {
        scene.general.resolve(bag);
        resolve_us(&mut scene.camera.fov, bag);
        for object in &mut scene.objects {
            object.resolve(bag);
        }
        SceneModel { scene }
    }

    pub fn reresolve(&mut self, bag: &PropertyBag) {
        self.scene.general.resolve(bag);
        resolve_us(&mut self.scene.camera.fov, bag);
        for object in &mut self.scene.objects {
            object.resolve(bag);
        }
    }

    pub fn load_assets(&mut self, source: &dyn AssetSource, bag: &PropertyBag) -> Vec<AssetProblem> {
        let mut problems = Vec::new();
        for object in &mut self.scene.objects {
            match &mut object.kind {
                ObjectKind::Image(img) => load_image_assets(img, source, &mut problems),
                ObjectKind::Particle(p) => load_particle_assets(p, source, &mut problems),
                _ => {}
            }
        }
        for object in &mut self.scene.objects {
            object.resolve(bag);
        }
        problems
    }
}

fn load_image_assets(img: &mut ImageObject, source: &dyn AssetSource, problems: &mut Vec<AssetProblem>) {
    if let Some(value) = load_json(source, &img.image, problems) {
        match ModelFile::from_value(&value) {
            Ok(model) => {
                if let Some(value) = load_json(source, &model.material, problems) {
                    img.material = Some(Material::from_value(&value));
                }
                img.model = Some(model);
            }
            Err(e) => problems.push(AssetProblem {
                path: img.image.clone(),
                reason: e.to_string(),
            }),
        }
    }
    for effect in &mut img.effects {
        if let Some(value) = load_json(source, &effect.file, problems) {
            let mut file = EffectFile::from_value(&value);
            for pass in &mut file.passes {
                if let Some(mat_path) = pass.material.clone()
                    && let Some(mat_value) = load_json(source, &mat_path, problems)
                {
                    pass.resolved = Some(Material::from_value(&mat_value));
                }
            }
            effect.resolved = Some(file);
        }
    }
}

fn load_particle_assets(p: &mut ParticleObject, source: &dyn AssetSource, problems: &mut Vec<AssetProblem>) {
    if let Some(path) = p.particle_file.clone()
        && let Some(value) = load_json(source, &path, problems)
    {
        p.system = ParticleSystem::from_value(&value);
    }
    if let Some(mat_path) = p.system.material.clone()
        && let Some(value) = load_json(source, &mat_path, problems)
    {
        p.system.resolved_material = Some(Material::from_value(&value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::property::PropertyValue;
    use crate::user::ScriptBinding;

    fn scripted_scale() -> UserSetting<f32> {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "minvalue".to_owned(),
            serde_json::json!({ "user": "newproperty44", "value": 0.8 }),
        );
        properties.insert(
            "maxvalue".to_owned(),
            serde_json::json!({ "user": "newproperty46", "value": 1.2 }),
        );
        UserSetting {
            value: 1.0,
            user: None,
            script: Some(ScriptBinding {
                source: "export function update() {}".to_owned(),
                properties,
            }),
        }
    }

    #[test]
    fn a_scripted_setting_takes_its_user_bound_properties() {
        let mut bag = PropertyBag::default();
        bag.insert("newproperty44", PropertyValue::Number(1.0));
        bag.insert("newproperty46", PropertyValue::Number(1.0));
        let mut us = scripted_scale();
        resolve_us(&mut us, &bag);
        let props = &us.script.as_ref().expect("a script").properties;
        assert_eq!(props["minvalue"], serde_json::json!(1.0));
        assert_eq!(props["maxvalue"], serde_json::json!(1.0));
    }

    #[test]
    fn a_scripted_setting_keeps_its_fallback_when_unbound() {
        let bag = PropertyBag::default();
        let mut us = scripted_scale();
        resolve_us(&mut us, &bag);
        let props = &us.script.as_ref().expect("a script").properties;
        assert_eq!(props["minvalue"], serde_json::json!(0.8));
    }
}
