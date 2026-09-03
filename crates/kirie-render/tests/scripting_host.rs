use kirie_render::scene::scripting::{PropTarget, ScriptHost};
use kirie_scene::{PropertyBag, Scene, SceneModel};

fn model(json: &str) -> SceneModel {
    let scene = Scene::from_slice(json.as_bytes()).expect("parse scene.json");
    SceneModel::resolve(scene, &PropertyBag::default())
}

#[test]
fn scripted_alpha_changes_over_ticks() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 7,
                "name": "layer",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { return engine.runtime; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("scene has a driveable script");

    let mut last = -1.0_f32;
    let mut saw_update = false;
    for _ in 0..4 {
        let updates = host.tick(0.5, None, [0.5, 0.5], [960.0, 540.0], false, None);
        for u in updates {
            if u.object_id == 7 && u.target == PropTarget::Alpha {
                let v = kirie_render::scene::scripting::as_f32(&u.value).expect("alpha is a scalar");
                assert!(v > last, "scripted alpha must increase each tick: {v} !> {last}");
                last = v;
                saw_update = true;
            }
        }
    }
    assert!(saw_update, "the alpha script produced no property update");
    assert!(
        last > 0.5,
        "runtime-driven alpha advanced past the first frame: {last}"
    );
}

#[test]
fn retained_frame_refreshes_user_props() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 64, "height": 64 } },
        "objects": [
            {
                "id": 5,
                "name": "layer",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { return engine.userProperties.mode == 'on' ? 0.9 : 0.1; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let props = vec![(
        "mode".to_owned(),
        kirie_scene::PropertyValue::Combo("off".to_owned()),
    )];
    let mut host = ScriptHost::build(&model, (64, 64), &props).expect("scene has a driveable script");

    let alpha = |updates: Vec<kirie_render::scene::scripting::PropUpdate>| {
        updates
            .into_iter()
            .find(|u| u.object_id == 5 && u.target == PropTarget::Alpha)
            .and_then(|u| kirie_render::scene::scripting::as_f32(&u.value))
            .expect("alpha update present")
    };

    assert!((alpha(host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None)) - 0.1).abs() < 1e-6);
    host.apply_user_property("mode", &kirie_scene::PropertyValue::Combo("on".to_owned()));
    assert!((alpha(host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None)) - 0.9).abs() < 1e-6);
    assert!((alpha(host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None)) - 0.9).abs() < 1e-6);
}

#[test]
fn text_writes_and_created_layer_writes_reach_updates() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 9,
                "name": "label",
                "text": "hi",
                "alpha": {
                    "value": 1.0,
                    "script": "var made = null; export function update(v) { thisLayer.text = 'tick ' + engine.runtime; if (!made) { made = thisScene.createLayer('models/glow.json'); } made.alpha = engine.runtime; return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");
    for t in 0..3 {
        let updates = host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
        assert!(
            updates.iter().any(|u| u.object_id == 9
                && u.target == PropTarget::Text
                && matches!(&u.value, kirie_script::ScriptValue::Str(s) if s.starts_with("tick "))),
            "tick {t}: no Text update for the thisLayer.text write"
        );
        assert!(
            updates
                .iter()
                .any(|u| u.object_id <= -1000 && u.target == PropTarget::Alpha),
            "tick {t}: created-layer alpha write was dropped"
        );
    }
}

#[test]
fn destroy_layer_drains_and_forgets_the_record() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 7,
                "name": "layer",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "var made = null, t = 0; export function update(v) { t++; if (t === 1) { made = thisScene.createLayer('models/glow.json'); } if (t === 2) { if (!thisScene.destroyLayer(made)) console.error('destroy failed'); } if (t === 3) { made.alpha = 0.5; } return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");

    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let created = host.take_created();
    assert_eq!(created.len(), 1, "created: {created:?}");
    let id = created[0].0;
    assert!(host.take_destroyed().is_empty());

    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert_eq!(host.take_destroyed(), vec![id]);

    let updates = host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert!(
        updates.iter().all(|u| u.object_id != id),
        "write to a destroyed layer must no-op: {updates:?}"
    );
}

#[test]
fn parallax_depth_write_reaches_updates() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 7,
                "name": "layer",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { thisLayer.parallaxDepth = new Vec2(2, 3); return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");
    let updates = host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert!(
        updates.iter().any(|u| u.object_id == 7
            && u.target == PropTarget::ParallaxDepth
            && kirie_render::scene::scripting::as_vec3(&u.value).is_some_and(|v| v[0] == 2.0 && v[1] == 3.0)),
        "no ParallaxDepth update: {updates:?}"
    );
}

#[test]
fn base_origin_leaf_script_is_collected() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 5,
                "name": "chaser",
                "image": "models/x.json",
                "origin": {
                    "value": "10 20 0",
                    "script": "export function update(v) { return new Vec3(input.cursorWorldPosition.x, input.cursorWorldPosition.y, 0); }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("origin script spawns the host");
    let updates = host.tick(0.5, None, [0.5, 0.5], [64.0, 48.0], false, None);
    assert!(
        updates.iter().any(|u| u.object_id == 5
            && u.target == PropTarget::Origin
            && kirie_render::scene::scripting::as_vec3(&u.value)
                .is_some_and(|v| v[0] == 64.0 && v[1] == 48.0)),
        "no Origin update from the base-leaf script: {updates:?}"
    );
}

#[test]
fn particle_ops_drain_with_payloads() {
    use kirie_render::scene::scripting::ParticleOp;
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 3,
                "name": "sys",
                "particle": "particles/x.json",
                "instanceoverride": {
                    "rate": {
                        "value": 1.0,
                        "script": "export function update(v) { thisLayer.pause(); thisLayer.emitParticles(5); thisLayer.instance.size = 2.5; thisLayer.instance.controlpoint1 = new Vec3(10, 20, 0); if (thisLayer.isPlaying()) { console.error('should be paused'); } return v; }"
                    }
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let ops = host.take_particle_ops();
    assert!(
        ops.iter()
            .any(|o| matches!(o, ParticleOp::Command { id: 3, cmd } if cmd == "pause")),
        "pause missing"
    );
    assert!(
        ops.iter()
            .any(|o| matches!(o, ParticleOp::Emit { id: 3, count: 5 })),
        "emit missing"
    );
    assert!(
        ops.iter().any(
            |o| matches!(o, ParticleOp::Instance { id: 3, name, value } if name == "size"
            && kirie_render::scene::scripting::as_f32(value) == Some(2.5))
        ),
        "size missing"
    );
    assert!(
        ops.iter().any(
            |o| matches!(o, ParticleOp::Instance { id: 3, name, value } if name == "controlpoint1"
            && kirie_render::scene::scripting::as_vec3(value).is_some_and(|v| v[0] == 10.0 && v[1] == 20.0))
        ),
        "controlpoint1 missing"
    );
}

#[test]
fn effect_material_writes_drain_as_ops() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 6,
                "name": "layer",
                "image": "models/x.json",
                "effects": [
                    { "file": "effects/glow.json", "name": "Glow" }
                ],
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { if (thisLayer.getEffectCount() !== 1) { console.error('count'); return v; } var e = thisLayer.getEffect('Glow'); if (!e || e.name !== 'Glow') { console.error('handle'); return v; } e.setMaterialProperty('g_Strength', 0.75); e.setMaterialProperty('g_Tint', new Vec3(1, 0, 0)); return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let ops = host.take_material_ops();
    assert!(
        ops.iter().any(|(id, eff, name, v)| *id == 6
            && *eff == 0
            && name == "g_Strength"
            && kirie_render::scene::scripting::as_f32(v) == Some(0.75)),
        "scalar write missing: {ops:?}"
    );
    assert!(
        ops.iter().any(|(id, eff, name, v)| *id == 6
            && *eff == 0
            && name == "g_Tint"
            && kirie_render::scene::scripting::as_vec3(v).is_some_and(|c| c == [1.0, 0.0, 0.0])),
        "vec write missing: {ops:?}"
    );
}

#[test]
fn video_control_calls_drain_as_ops() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 8,
                "name": "vid",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { var t = thisLayer.getVideoTexture(); t.pause(); t.rate = 0.5; if (t.isPlaying()) { console.error('should be paused'); } return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let ops = host.take_video_ops();
    assert!(
        ops.iter().any(|(id, cmd, _)| *id == 8 && cmd == "pause"),
        "pause missing: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|(id, cmd, v)| *id == 8 && cmd == "rate" && (*v - 0.5).abs() < 1e-9),
        "rate missing: {ops:?}"
    );
}

#[test]
fn camera_echo_carries_the_synced_fov() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0", "fov": 50.0 },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 35,
                "name": "cam",
                "image": "models/x.json",
                "visible": {
                    "value": true,
                    "script": "export function update(v) { thisScene.setCameraTransforms(thisScene.getCameraTransforms()); return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("host");
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert_eq!(host.take_camera().and_then(|c| c.fov), Some(50.0));
    host.set_scene_fov(36.9);
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert_eq!(host.take_camera().and_then(|c| c.fov), Some(36.9));
}

#[test]
fn effect_constant_script_routes_to_material_ops() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 45,
                "name": "Effects/Coloring",
                "image": "models/x.json",
                "effects": [
                    {
                        "file": "effects/coloring.json",
                        "passes": [
                            {
                                "constantshadervalues": {
                                    "color": {
                                        "value": "1 0 0",
                                        "script": "export function update(v) { return new Vec3(0, engine.runtime > 0 ? 1 : 0, 0); }"
                                    }
                                }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("effect constant script spawns host");
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let ops = host.take_material_ops();
    assert!(
        ops.iter().any(|(id, ei, name, v)| *id == 45
            && *ei == 0
            && name == "color"
            && kirie_render::scene::scripting::as_vec3(v).is_some_and(|c| c == [0.0, 1.0, 0.0])),
        "constant result not routed: {ops:?}"
    );
}

#[test]
fn scene_without_scripts_spawns_no_host() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 64, "height": 64 } },
        "objects": [
            { "id": 1, "name": "plain", "image": "models/x.json", "alpha": { "value": 0.5 } }
        ]
    }"#;
    let model = model(json);
    assert!(
        ScriptHost::build(&model, (64, 64), &[]).is_none(),
        "no script binding ⇒ no engine thread (V9 best-effort)"
    );
}

#[test]
fn throwing_script_does_not_panic_and_leaves_value_alone() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 64, "height": 64 } },
        "objects": [
            {
                "id": 3,
                "name": "boom",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { throw new Error('boom'); }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (64, 64), &[]).expect("script loads even if it throws at tick");
    for _ in 0..3 {
        let updates = host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None);
        assert!(
            updates.iter().all(|u| u.object_id != 3),
            "a throwing update must not apply a value"
        );
    }
}

#[test]
fn angles_cross_the_script_boundary_in_degrees() {
    let json = r#"{
        "camera": { "eye": "0 0 100", "center": "0 0 0", "up": "0 1 0" },
        "general": { "orthogonalprojection": { "width": 128, "height": 128 } },
        "objects": [
            {
                "id": 3,
                "name": "dial",
                "image": "models/x.json",
                "angles": {
                    "value": "0 0 -5.68977",
                    "script": "export function update(v) { return new Vec3(0, 0, Math.round(v.z) + 90); }"
                }
            },
            {
                "id": 4,
                "name": "hand",
                "image": "models/x.json",
                "alpha": {
                    "value": 1.0,
                    "script": "export function update(v) { var d = thisScene.getLayer('dial'); if (Math.round(d.angles.z) == -326) thisLayer.angles = new Vec3(0, 0, 180); return v; }"
                }
            }
        ]
    }"#;
    let model = model(json);
    let mut host = ScriptHost::build(&model, (128, 128), &[]).expect("scene has a driveable script");
    let updates = host.tick(0.016, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let angle = |id: i64| {
        updates
            .iter()
            .find(|u| u.object_id == id && u.target == PropTarget::Angles)
            .and_then(|u| kirie_render::scene::scripting::as_vec3(&u.value))
            .unwrap_or_else(|| panic!("angles update for {id}"))[2]
    };
    assert!(
        (angle(3) - (-236.0_f32).to_radians()).abs() < 1e-3,
        "dial: {}",
        angle(3)
    );
    assert!(
        (angle(4) - std::f32::consts::PI).abs() < 1e-5,
        "hand: {}",
        angle(4)
    );
}
