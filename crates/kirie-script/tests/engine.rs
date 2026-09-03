use kirie_script::{
    AnimationState, AudioBuffers, HostFrame, LayerState, MediaFrame, SceneOp, ScriptEngine, ScriptValue,
};

fn num(v: &ScriptValue) -> f64 {
    match v {
        ScriptValue::Int(i) => *i as f64,
        ScriptValue::Float(f) => *f,
        other => panic!("expected number, got {other:?}"),
    }
}

#[test]
fn vec_math_correct_and_operand_order_fixed() {
    let e = ScriptEngine::new().unwrap();
    assert_eq!(
        e.eval("new Vec3(5,5,5).subtract(new Vec3(1,2,3)).x").unwrap(),
        "4"
    );
    assert_eq!(e.eval("new Vec2(10,10).divide(new Vec2(2,5)).y").unwrap(), "2");
    assert_eq!(e.eval("new Vec3(1,0,0).cross(new Vec3(0,1,0)).z").unwrap(), "1");
    assert_eq!(
        e.eval("new Vec3(0,0,0).mix(new Vec3(10,10,10), 0.5).x").unwrap(),
        "5"
    );
    assert_eq!(e.eval("new Vec2(3,4).length()").unwrap(), "5");
    assert_eq!(e.eval("new Vec2(3,4).lengthSqr()").unwrap(), "25");
    assert_eq!(
        e.eval("new Vec3(1,2,3).add(new Vec3(1,1,1)).toString()").unwrap(),
        "2.000000, 3.000000, 4.000000"
    );
}

#[test]
fn mat4_transform_and_compose() {
    let e = ScriptEngine::new().unwrap();
    assert_eq!(
        e.eval("Mat4.fromTranslation(new Vec3(5,6,7)).transformPoint(new Vec3(0,0,0)).x")
            .unwrap(),
        "5"
    );
    assert_eq!(
        e.eval("Mat4.fromScale(2).transformPoint(new Vec3(3,0,0)).x")
            .unwrap(),
        "6"
    );
    assert_eq!(
        e.eval("Math.round(Mat4.fromRotation(90, new Vec3(0,0,1)).transformDirection(new Vec3(1,0,0)).y)")
            .unwrap(),
        "1"
    );
}

#[test]
fn console_and_localstorage() {
    let e = ScriptEngine::new().unwrap();
    assert_eq!(
        e.eval("localStorage.set('k','v'); localStorage.get('k')")
            .unwrap(),
        "v"
    );
    assert_eq!(e.eval("localStorage.get('missing')").unwrap(), "null");
    assert_eq!(
        e.eval("localStorage.set('n', 42); localStorage.get('n')")
            .unwrap(),
        "42"
    );
    assert_eq!(e.eval("MediaPlaybackEvent.PLAYBACK_PLAYING").unwrap(), "1");
}

#[test]
fn update_return_applied_to_property() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "text_1",
        "export function update(value){ return 'hello ' + value; }",
        None,
        ScriptValue::Str("world".into()),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results.len(), 1);
    assert_eq!(out.property_results[0].0, "text_1");
    assert_eq!(out.property_results[0].1, ScriptValue::Str("hello world".into()));
    assert!(out.errors.is_empty());
}

#[test]
fn init_runs_once_before_update() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_2",
        "let n = 0; export function init(v){ n = 100; } export function update(v){ n += 1; return n; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let a = e.tick(HostFrame::default(), vec![]).unwrap();
    let b = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(num(&a.property_results[0].1), 101.0);
    assert_eq!(num(&b.property_results[0].1), 102.0);
}

#[test]
fn script_properties_from_json_only() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "text_9",
        "export var scriptProperties = createScriptProperties().addCombo({name:'monthFormat',value:'99'}).finish();\
         export function update(v){ return (scriptProperties.monthFormat == 1) ? 'numeric' : 'other'; }",
        None,
        ScriptValue::Str(String::new()),
        serde_json::json!({ "monthFormat": "1" }),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("numeric".into()));
}

#[test]
fn throwing_update_is_typed_error_not_panic() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "color_3",
        "export function update(v){ throw new Error('boom'); }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert!(out.property_results.is_empty(), "write-back skipped on throw");
    assert_eq!(out.errors.len(), 1);
    assert!(matches!(
        &out.errors[0],
        kirie_script::ScriptError::Runtime { phase: "update", .. }
    ));
    assert_eq!(e.eval("1+1").unwrap(), "2");
}

#[test]
fn malformed_source_is_load_error_not_panic() {
    let e = ScriptEngine::new().unwrap();
    let r = e.load_property_script(
        "broken_4",
        "export function update(v { this is not valid",
        None,
        ScriptValue::Null,
        serde_json::json!({}),
    );
    assert!(matches!(r, Err(kirie_script::ScriptError::Load { .. })));
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert!(out.property_results.is_empty());
}

#[test]
fn apply_user_properties_fires_on_every_module() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "rate_5",
        "export function applyUserProperties(changed){ if ('foo' in changed) console.log('upd:' + changed.foo); }\
         export function update(v){ return v; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.dispatch_user_property("foo", ScriptValue::Int(7)).unwrap();
    assert!(
        out.logs.iter().any(|l| l.message == "upd:7"),
        "logs: {:?}",
        out.logs
    );
}

#[test]
fn we_modules_import_and_compute() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "a_6",
        "import * as WEMath from 'WEMath';\
         import * as WEColor from 'WEColor';\
         export function update(v){ return WEMath.mix(0, 10, 0.5) + WEColor.hsv2rgb(new Vec3(0,1,1)).x; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(num(&out.property_results[0].1), 6.0);
}

#[test]
fn this_layer_write_records_scene_op() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "visible_42",
        "export function update(v){ thisLayer.visible = false; return v; }",
        Some(42),
        ScriptValue::Bool(true),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        layers: vec![LayerState {
            id: 42,
            name: "L".into(),
            visible: Some(true),
            ..Default::default()
        }],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert!(
        out.ops.iter().any(|op| matches!(op,
        SceneOp::SetProperty { layer_id: 42, name, value: ScriptValue::Bool(false) } if name == "visible")),
        "ops: {:?}",
        out.ops
    );
}

#[test]
fn this_scene_write_records_scene_op() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "visible_7",
        "export function init(){ thisScene.cameraparallax = true; thisScene.cameraparallaxdelay = 0.61; thisScene.clearcolor = new Vec3(0.5, 0.25, 0); }\
         export function update(v){ return thisScene.cameraparallax && thisScene.clearcolor.x === 0.5; }",
        Some(7),
        ScriptValue::Bool(false),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.property_results[0].1, ScriptValue::Bool(true));
    assert!(out.ops.iter().any(|op| matches!(op,
        SceneOp::SetSceneProperty { name, value: ScriptValue::Bool(true) } if name == "cameraparallax")));
    assert!(out.ops.iter().any(|op| matches!(op,
        SceneOp::SetSceneProperty { name, value: ScriptValue::Float(f) } if name == "cameraparallaxdelay" && (f - 0.61).abs() < 1e-9)));
    assert!(out.ops.iter().any(|op| matches!(op,
        SceneOp::SetSceneProperty { name, value: ScriptValue::Vec3(v) } if name == "clearcolor" && *v == [0.5, 0.25, 0.0])));
}

#[test]
fn cursor_screen_position_is_in_pixels() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "export function update(v){ return input.cursorScreenPosition.x + input.cursorScreenPosition.y; }",
        Some(7),
        ScriptValue::Float(0.0),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        res_x: 1920.0,
        res_y: 1080.0,
        pointer_screen: [0.5, 0.25],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    match &out.property_results[0].1 {
        ScriptValue::Float(f) => assert!((f - (960.0 + 270.0)).abs() < 1e-6, "got {f}"),
        other => panic!("expected float, got {other:?}"),
    }
}

#[test]
fn screen_resolution_and_canvas_size_are_vec2() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "export function update(v){
            var ok = engine.screenResolution instanceof Vec2 && engine.canvasSize instanceof Vec2;
            return ok ? engine.screenResolution.divide(2).x : -1;
        }",
        Some(7),
        ScriptValue::Float(0.0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(960));
}

const CURSOR_LOG_SCRIPT: &str = "var log = [];
export function cursorEnter(ev){ log.push('enter:' + ev.localPosition.x); }
export function cursorLeave(ev){ log.push('leave'); }
export function cursorDown(ev){ log.push('down'); }
export function cursorUp(ev){ log.push('up'); }
export function cursorClick(ev){ log.push('click:' + ev.worldPosition.x); }
export function update(v){ return log.join(','); }";

fn cursor_layer(solid: Option<bool>) -> LayerState {
    LayerState {
        id: 42,
        name: "L".into(),
        origin: Some([100.0, 100.0, 0.0]),
        scale: Some([1.0; 3]),
        size: Some([50.0, 50.0]),
        solid,
        ..Default::default()
    }
}

fn cursor_frame(solid: Option<bool>, px: f32, py: f32, left: bool) -> HostFrame {
    HostFrame {
        layers: vec![cursor_layer(solid)],
        pointer_world: [px, py, 0.0],
        pointer_left_down: left,
        ..Default::default()
    }
}

#[test]
fn cursor_events_fire_on_solid_layer_edges() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_42",
        CURSOR_LOG_SCRIPT,
        Some(42),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let solid = Some(true);
    let out = e.tick(cursor_frame(solid, 0.0, 0.0, false), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str(String::new()));
    let out = e.tick(cursor_frame(solid, 110.0, 95.0, false), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("enter:10".into()));
    let out = e.tick(cursor_frame(solid, 110.0, 95.0, true), vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("enter:10,down".into())
    );
    let out = e.tick(cursor_frame(solid, 110.0, 95.0, false), vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("enter:10,down,up,click:110".into())
    );
    let out = e.tick(cursor_frame(solid, 0.0, 0.0, false), vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("enter:10,down,up,click:110,leave".into())
    );
}

#[test]
fn cursor_events_require_the_solid_flag() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_42",
        CURSOR_LOG_SCRIPT,
        Some(42),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    e.tick(cursor_frame(None, 0.0, 0.0, false), vec![]).unwrap();
    e.tick(cursor_frame(None, 110.0, 95.0, true), vec![]).unwrap();
    let out = e.tick(cursor_frame(None, 110.0, 95.0, false), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str(String::new()));
}

#[test]
fn audio_reads_silent_after_a_frame_without_bands() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_4",
        "var b = engine.registerAudioBuffers(16);
         export function update(v){ return b.average[0]; }",
        Some(4),
        ScriptValue::Float(0.0),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        audio: Some(AudioBuffers {
            audio16: vec![0.75; 16],
            audio32: vec![0.75; 32],
            audio64: vec![0.75; 64],
        }),
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Float(0.75));
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(0));
}

#[test]
fn initial_layer_config_survives_script_writes() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_42",
        "var t = 0;
         export function update(v){
            t++;
            if (t === 1) { thisLayer.alpha = 0.9; return v; }
            var c = thisScene.getInitialLayerConfig(thisLayer);
            return c ? c.alpha : -1;
         }",
        Some(42),
        ScriptValue::Float(0.0),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = || HostFrame {
        layers: vec![LayerState {
            id: 42,
            name: "L".into(),
            alpha: Some(0.25),
            ..Default::default()
        }],
        ..Default::default()
    };
    e.tick(frame(), vec![]).unwrap();
    let mut mutated = frame();
    mutated.layers[0].alpha = Some(0.9);
    let out = e.tick(mutated, vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Float(0.25));
}

#[test]
fn media_events_dispatch_by_category() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "var log = [];
         export function mediaPlaybackChanged(ev){ log.push('pb:' + ev.state); }
         export function mediaThumbnailChanged(ev){ log.push('th:' + (ev.primaryColor instanceof Vec3 ? ev.primaryColor.x : 'bad')); }
         export function mediaStatusChanged(ev){ log.push('st:' + ev.enabled); }
         export function update(v){ return log.join(','); }",
        Some(7),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let media = MediaFrame {
        enabled: true,
        state: 1,
        colors: Some([[1.0, 0.0, 0.0]; 5]),
        playback_changed: true,
        thumbnail_changed: true,
        status_changed: false,
        ..Default::default()
    };
    let frame = HostFrame {
        media: Some(media),
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("pb:1,th:1".into()),
        "errors: {:?}",
        out.errors
    );
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("pb:1,th:1".into()));
}

#[test]
fn resize_screen_fires_on_resolution_change() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "var w = 0;
         export function resizeScreen(size){ w = (size instanceof Vec2) ? size.x : -1; }
         export function update(v){ return w; }",
        Some(7),
        ScriptValue::Float(0.0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(0));
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(0));
    let frame = HostFrame {
        res_x: 2560.0,
        res_y: 1440.0,
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(2560));
}

#[test]
fn apply_general_settings_delivers_language_at_load() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "var lang = '';
         export function applyGeneralSettings(s){ if (s.hasOwnProperty('language')) lang = s.language; }
         export function update(v){ return lang; }",
        Some(7),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    match &out.property_results[0].1 {
        ScriptValue::Str(s) => assert!(!s.is_empty(), "language must be non-empty"),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn math_surface_matches_hand_computed_values() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_1",
        r#"
        function close(a, b) { return Math.abs(a - b) < 1e-4; }
        export function update(v) {
            var bad = [];
            // Vec4 reflect: (1,0,0,0) off normal (0,1,0,0) is itself.
            var r = new Vec4(1, 2, 0, 0).reflect(new Vec4(0, 1, 0, 0));
            if (!(close(r.x, 1) && close(r.y, -2))) bad.push('v4reflect');
            // Mat3 inverse: M * M^-1 = I.
            var m3 = Mat3.compose(new Vec2(3, 4), 30, new Vec2(2, 2));
            var i3 = m3.multiply(m3.inverse());
            if (!i3.equals(Mat3.identity())) bad.push('m3inverse');
            // Mat3 decompose round-trips.
            var d3 = m3.decompose();
            if (!(close(d3.translation.x, 3) && close(d3.rotation, 30) && close(d3.scale.x, 2))) bad.push('m3decompose');
            // Mat4 inverse: M * M^-1 = I.
            var m4 = Mat4.compose(new Vec3(1, 2, 3), new Vec3(10, 20, 30), new Vec3(2, 3, 4));
            if (!m4.multiply(m4.inverse()).equals(Mat4.identity())) bad.push('m4inverse');
            // Mat4 determinant of a pure scale = product of scales.
            if (!close(Mat4.fromScale(new Vec3(2, 3, 4)).determinant(), 24)) bad.push('m4det');
            // extractEuler inverts fromEuler.
            var eu = Mat4.fromEuler(10, 20, 30).extractEuler();
            if (!(close(eu.x, 10) && close(eu.y, 20) && close(eu.z, 30))) bad.push('m4euler');
            // lookAt from origin toward -Z with +Y up is identity.
            if (!Mat4.lookAt(new Vec3(0, 0, 0), new Vec3(0, 0, -1), new Vec3(0, 1, 0)).equals(Mat4.identity())) bad.push('m4lookat');
            return bad.length ? bad.join(',') : 'ok';
        }
        "#,
        Some(1),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("ok".into()),
        "errors: {:?}",
        out.errors
    );
}

#[test]
fn local_storage_persists_across_engines() {
    let dir = std::env::temp_dir().join(format!("kirie-storage-test-{}", std::process::id()));
    let path = dir.join("test.json");
    let _ = std::fs::remove_file(&path);

    let e = ScriptEngine::new().unwrap();
    e.set_storage_path(path.clone()).unwrap();
    e.load_property_script(
        "alpha_1",
        "export function update(v){
            localStorage.set('mode', 'night');
            return localStorage.delete('missing') ? 'bad' : localStorage.get('mode');
        }",
        Some(1),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("night".into()));
    drop(e);
    assert!(path.exists(), "storage file written");

    let e2 = ScriptEngine::new().unwrap();
    e2.set_storage_path(path.clone()).unwrap();
    e2.load_property_script(
        "alpha_1",
        "export function update(v){ return localStorage.get('mode') || 'empty'; }",
        Some(1),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e2.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("night".into()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stubbed_surface_never_crashes() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_1",
        r#"
        export function update(v) {
            var bad = [];
            if (thisLayer.getAnimationLayerCount() !== 0) bad.push('alc');
            var al = thisLayer.playSingleAnimation('walk');
            al.play(); al.setFrame(3);
            if (!al.isPlaying() || al.getFrame() !== 3) bad.push('al');
            if (!(thisLayer.getBoneTransform('head') instanceof Mat4)) bad.push('bone');
            if (!(thisLayer.getAttachmentOrigin(0) instanceof Vec3)) bad.push('attach');
            if (!(thisLayer.transformAttachmentToTexture(0, 0) instanceof Mat3)) bad.push('t2t');
            thisLayer.volume = 0.5; thisLayer.applyData([]);
            var md = thisScene.createModelData({});
            if (!md || md.__modelData !== true) bad.push('md');
            if (thisScene.destroyModelData(md) !== false) bad.push('dmd');
            if (typeof renderContext !== 'object') bad.push('rc');
            return bad.length ? bad.join(',') : 'ok';
        }
        "#,
        Some(1),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        layers: vec![LayerState {
            id: 1,
            name: "L".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("ok".into()),
        "errors: {:?} logs: {:?}",
        out.errors,
        out.logs
    );
    assert!(out.logs.iter().any(|l| l.message.contains("not simulated")));
}

#[test]
fn engine_interval_fires_by_frame_clock() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "export function update(v){ if (v === 0) { engine.setInterval(function(){ console.log('fire'); }, 100); } return 1; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let t1 = e
        .tick(
            HostFrame {
                now: 0.0,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    assert!(!t1.logs.iter().any(|l| l.message == "fire"));
    let t2 = e
        .tick(
            HostFrame {
                now: 200.0,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    assert!(t2.logs.iter().any(|l| l.message == "fire"), "logs: {:?}", t2.logs);
}

#[test]
fn text_layer_script_ticks_and_reads_text() {
    let e = ScriptEngine::new().unwrap();
    let h = e
        .create_layer_script(
            "'use strict';\nexport function update(value){ return 'T:' + Math.floor(thisScene.time); }",
            serde_json::json!({}),
            "placeholder",
        )
        .unwrap();
    assert!(h > 0);
    e.tick_layer(h, 5.0, 0.016, 60.0).unwrap();
    assert_eq!(e.layer_text(h).unwrap(), "T:5");
    e.destroy_layer(h).unwrap();
    assert_eq!(e.layer_text(h).unwrap(), "");
}

#[test]
fn register_audio_buffers_reads_matching_resolution() {
    let e = ScriptEngine::new().unwrap();
    for (res, key) in [(16, "a16"), (32, "a32"), (64, "a64")] {
        e.load_property_script(
            format!("alpha_{res}"),
            format!(
                "export function update(v){{ var a = engine.registerAudioBuffers({res}).average; \
                 var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return a.length * 1000 + s; }}"
            ),
            None,
            ScriptValue::Float(0.0),
            serde_json::json!({}),
        )
        .unwrap();
        let _ = key;
    }
    let audio = AudioBuffers {
        audio16: vec![0.5; 16],
        audio32: vec![0.25; 32],
        audio64: vec![0.1; 64],
    };
    let out = e
        .tick(
            HostFrame {
                audio: Some(audio),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let get = |k: &str| -> f64 {
        num(&out
            .property_results
            .iter()
            .find(|(key, _)| key == k)
            .expect("result")
            .1)
    };
    assert!(
        (get("alpha_16") - 16_008.0).abs() < 1e-3,
        "16-band read wrong array"
    );
    assert!(
        (get("alpha_32") - 32_008.0).abs() < 1e-3,
        "32-band read wrong array"
    );
    assert!(
        (get("alpha_64") - 64_006.4).abs() < 1e-2,
        "64-band read wrong array"
    );
}

#[test]
fn register_audio_buffers_silent_is_zeroed() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_1",
        "export function update(v){ var a = engine.registerAudioBuffers(32).average; \
         var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return a.length * 1000 + s; }",
        None,
        ScriptValue::Float(0.0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(
        num(&out.property_results[0].1),
        32_000.0,
        "silent 32-band all zeros"
    );
}

#[test]
fn version_constants_exposed() {
    assert_eq!(kirie_script::API_VERSION, "2.8");
    assert_eq!(kirie_script::TRANSLATOR_VERSION, 1);
}

#[test]
fn update_receives_the_live_vector_prop_as_a_vec3() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "scale_9",
        "import * as WEMath from 'WEMath';\
         export function update(v){ return new Vec3(WEMath.mix(v.x, 1, 0.5), v.y, v.z); }",
        Some(9),
        ScriptValue::Vec3([2.0, 2.0, 2.0]),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        layers: vec![LayerState {
            id: 9,
            name: "L".into(),
            scale: Some([3.0, 4.0, 5.0]),
            ..Default::default()
        }],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.property_results[0].1, ScriptValue::Vec3([2.0, 4.0, 5.0]));
}

#[test]
fn we_math_matches_the_shipped_module() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_3",
        "import * as WEMath from 'WEMath';\
         export function update(v){ return new Vec3(WEMath.smoothStep(0, 10, 5), WEMath.mix(2, 4, 0.25), Number.isNaN(WEMath.mix(undefined, 1, 0.1)) ? 1 : 0); }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.property_results[0].1, ScriptValue::Vec3([0.5, 2.5, 1.0]));
}

#[test]
fn this_object_aliases_this_layer() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_5",
        "export function update(v){ return thisObject === thisLayer && thisObject.name === 'L' ? 1 : 0; }",
        Some(5),
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        layers: vec![LayerState {
            id: 5,
            name: "L".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(num(&out.property_results[0].1), 1.0);
}

fn anim(id: i64, key: &str, name: &str) -> AnimationState {
    AnimationState {
        id,
        key: key.into(),
        name: name.into(),
        fps: 30.0,
        frames: 60.0,
        duration: 2.0,
        rate: 1.0,
        playing: false,
        frame: 12.0,
    }
}

#[test]
fn animation_handles_resolve_by_property_and_name() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_7",
        "var log = [];
         export function init(){
           var own = thisObject.getAnimation();
           var named = thisScene.getAnimation('fade');
           var eff = thisLayer.getEffect(0).getAnimation();
           log.push(own ? own.name + ':' + own.frameCount + ':' + own.duration + ':' + own.getFrame() : 'none');
           log.push(named ? named.fps : 'none');
           log.push(eff ? eff.name : 'none');
           log.push(thisScene.getAnimation('missing') === undefined);
           own.play(); log.push(own.isPlaying());
           named.setFrame(29); named.rate = 2; log.push(named.getFrame() + ':' + named.rate);
           eff.stop();
         }
         export function update(v){ return log.join(','); }",
        Some(7),
        ScriptValue::Str(String::new()),
        serde_json::json!({}),
    )
    .unwrap();
    let frame = HostFrame {
        animations: vec![
            anim(7, "alpha_7", "own"),
            anim(9, "alpha_9", "fade"),
            anim(7, "fx0multiply_7", "glow"),
        ],
        layers: vec![LayerState {
            id: 7,
            name: "L".into(),
            effects: Some(vec![("glow".into(), 1)]),
            ..Default::default()
        }],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("own:60:2:12,30,glow,true,true,29:2".into())
    );
    let cmds: Vec<_> = out
        .ops
        .iter()
        .filter_map(|op| match op {
            SceneOp::AnimationCommand { index, cmd, value } => Some((*index, cmd.as_str(), *value)),
            _ => None,
        })
        .collect();
    assert_eq!(
        cmds,
        vec![
            (0, "play", 0.0),
            (1, "frame", 29.0),
            (1, "rate", 2.0),
            (2, "stop", 0.0)
        ]
    );
}

#[test]
fn animation_events_reach_the_owning_object() {
    let e = ScriptEngine::new().unwrap();
    for (key, id) in [("alpha_7", 7), ("visible_7", 7), ("alpha_8", 8)] {
        e.load_property_script(
            key,
            "var seen = '';
             export function animationEvent(ev, value){ seen += ev.name + '@' + ev.frame + '/' + value + ';'; }
             export function update(v){ return seen; }",
            Some(id),
            ScriptValue::Str("base".into()),
            serde_json::json!({}),
        )
        .unwrap();
    }
    let _ = e.tick(HostFrame::default(), vec![]).unwrap();
    let frame = HostFrame {
        animation_events: vec![(7, "yeshu".into(), 30.0)],
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    let by_key = |k: &str| {
        out.property_results
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(by_key("alpha_7"), ScriptValue::Str("yeshu@30/;".into()));
    assert_eq!(by_key("visible_7"), ScriptValue::Str("yeshu@30/;".into()));
    assert_eq!(by_key("alpha_8"), ScriptValue::Str(String::new()));
}
