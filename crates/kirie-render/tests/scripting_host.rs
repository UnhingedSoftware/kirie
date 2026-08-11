//! SceneScript host integration (docs/scripting-api.md §3/§5; SPEC.md §V3).
//!
//! GPU-free: builds a resolved [`SceneModel`] from an inline scene.json with a
//! scripted property, drives [`ScriptHost`] over several ticks, and asserts the
//! property value evolves — the "a scripted scene's property changes over ticks"
//! gate. Also asserts a script-free scene spawns no engine.

use kirie_render::scene::scripting::{PropTarget, ScriptHost};
use kirie_scene::{PropertyBag, Scene, SceneModel};

/// Resolve an inline scene.json into a [`SceneModel`] (no assets loaded — the
/// host only reads property/script bindings).
fn model(json: &str) -> SceneModel {
    let scene = Scene::from_slice(json.as_bytes()).expect("parse scene.json");
    SceneModel::resolve(scene, &PropertyBag::default())
}

#[test]
fn scripted_alpha_changes_over_ticks() {
    // An image object whose `alpha` is script-driven: `update` returns the
    // engine runtime, so the applied value grows every frame (docs §5.1).
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
    // The host recycles one boxed `HostFrame` across ticks and only re-clones
    // `engine.userProperties` into it when a live `setProperty` marked it dirty
    // — this asserts a stale retained copy never survives the refresh, and
    // that the refreshed copy persists on later (clean) ticks.
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

    // Initial props ('off') through the fresh frame.
    assert!((alpha(host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None)) - 0.1).abs() < 1e-6);
    // Live setProperty flips the combo; the recycled frame must see it.
    host.apply_user_property("mode", &kirie_scene::PropertyValue::Combo("on".to_owned()));
    assert!((alpha(host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None)) - 0.9).abs() < 1e-6);
    // And keep seeing it on later clean (non-dirty) ticks.
    assert!((alpha(host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None)) - 0.9).abs() < 1e-6);
}

/// `thisLayer.text = …` from a property script must surface as a Text
/// property update (the renderer routes it to the re-rasterize seam) — and
/// writes to a script-created layer must keep flowing on ticks after the
/// creation tick (the synthetic record persists in the layer snapshot).
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

/// `thisScene.destroyLayer` drains the id (take_destroyed), removes the
/// record from the next snapshot, and later writes to the dead proxy no-op.
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

    // Tick 1: created.
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    let created = host.take_created();
    assert_eq!(created.len(), 1, "created: {created:?}");
    let id = created[0].0;
    assert!(host.take_destroyed().is_empty());

    // Tick 2: destroyed — id drained exactly once.
    host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert_eq!(host.take_destroyed(), vec![id]);

    // Tick 3: writing through the dead proxy produces no update for it.
    let updates = host.tick(0.5, None, [0.5, 0.5], [64.0, 64.0], false, None);
    assert!(
        updates.iter().all(|u| u.object_id != id),
        "write to a destroyed layer must no-op: {updates:?}"
    );
}

/// `thisLayer.parallaxDepth = Vec2` surfaces as a ParallaxDepth update
/// (d.ts IImageLayer types it Vec2).
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

/// A script bound to the base `origin` leaf loads and drives Origin updates
/// (the cursor-follow pattern), even though it is not a kind-level leaf.
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
            && kirie_render::scene::scripting::as_vec3(&u.value).is_some_and(|v| v[0] == 64.0 && v[1] == 48.0)),
        "no Origin update from the base-leaf script: {updates:?}"
    );
}

/// Particle playback and instance writes drain as typed particle ops
/// (d.ts: ILayer extends IParticleSystem).
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
        ops.iter().any(|o| matches!(o, ParticleOp::Command { id: 3, cmd } if cmd == "pause")),
        "pause missing"
    );
    assert!(
        ops.iter().any(|o| matches!(o, ParticleOp::Emit { id: 3, count: 5 })),
        "emit missing"
    );
    assert!(
        ops.iter().any(|o| matches!(o, ParticleOp::Instance { id: 3, name, value } if name == "size"
            && kirie_render::scene::scripting::as_f32(value) == Some(2.5))),
        "size missing"
    );
    assert!(
        ops.iter().any(|o| matches!(o, ParticleOp::Instance { id: 3, name, value } if name == "controlpoint1"
            && kirie_render::scene::scripting::as_vec3(value).is_some_and(|v| v[0] == 10.0 && v[1] == 20.0))),
        "controlpoint1 missing"
    );
}

/// `getEffect(...)` resolves by name/index and `setMaterialProperty` drains
/// as a typed material op with the layer/effect/name/value payload.
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
    // A script that throws inside update surfaces as a typed error, never a
    // panic; the tick returns no update for it (SPEC.md §V9).
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
    // Several ticks, no panic; a throwing update yields no applied value.
    for _ in 0..3 {
        let updates = host.tick(0.016, None, [0.5, 0.5], [960.0, 540.0], false, None);
        assert!(
            updates.iter().all(|u| u.object_id != 3),
            "a throwing update must not apply a value"
        );
    }
}
