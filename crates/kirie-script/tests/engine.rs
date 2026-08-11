//! End-to-end tests: real JS through the embedded QuickJS runtime.
//! docs/scripting-api.md is the behavior oracle.

use kirie_script::{
    AudioBuffers, HostFrame, LayerState, MediaFrame, SceneOp, ScriptEngine, ScriptValue,
};

fn num(v: &ScriptValue) -> f64 {
    match v {
        ScriptValue::Int(i) => *i as f64,
        ScriptValue::Float(f) => *f,
        other => panic!("expected number, got {other:?}"),
    }
}

// ---- builtins: Vec/Mat math (docs §9 fixed + §10) -------------------------

#[test]
fn vec_math_correct_and_operand_order_fixed() {
    let e = ScriptEngine::new().unwrap();
    // add is commutative; subtract/divide/cross/mix use fixed operand order.
    assert_eq!(
        e.eval("new Vec3(5,5,5).subtract(new Vec3(1,2,3)).x").unwrap(),
        "4"
    ); // this - v
    assert_eq!(e.eval("new Vec2(10,10).divide(new Vec2(2,5)).y").unwrap(), "2"); // this / v
    assert_eq!(e.eval("new Vec3(1,0,0).cross(new Vec3(0,1,0)).z").unwrap(), "1"); // this × v
    assert_eq!(
        e.eval("new Vec3(0,0,0).mix(new Vec3(10,10,10), 0.5).x").unwrap(),
        "5"
    );
    assert_eq!(e.eval("new Vec2(3,4).length()").unwrap(), "5");
    assert_eq!(e.eval("new Vec2(3,4).lengthSqr()").unwrap(), "25"); // not aliased to length
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
    // 90° about Z maps +X to +Y.
    assert_eq!(
        e.eval("Math.round(Mat4.fromRotation(90, new Vec3(0,0,1)).transformDirection(new Vec3(1,0,0)).y)")
            .unwrap(),
        "1"
    );
}

// ---- console + localStorage (docs §6.5 / §10.3) ---------------------------

#[test]
fn console_and_localstorage() {
    let e = ScriptEngine::new().unwrap();
    // localStorage round-trips; missing key => null.
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
    // MediaPlaybackEvent constants present.
    assert_eq!(e.eval("MediaPlaybackEvent.PLAYBACK_PLAYING").unwrap(), "1");
}

// ---- property script contract (docs §5.1) ---------------------------------

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
    assert_eq!(num(&a.property_results[0].1), 101.0); // init(100) then +1
    assert_eq!(num(&b.property_results[0].1), 102.0); // init did NOT run again
}

#[test]
fn script_properties_from_json_only() {
    // docs §5.5: createScriptProperties descriptors are ignored; values come
    // from JSON scriptproperties. `==` string/number coercion (corpus).
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

// ---- V9: throwing script yields a typed error, never a panic --------------

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
    // Engine still alive.
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
    // A dropped script does not tick.
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert!(out.property_results.is_empty());
}

// ---- events: applyUserProperties (docs §5.3) ------------------------------

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

// ---- importable modules (docs §6.6) ---------------------------------------

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
    assert_eq!(num(&out.property_results[0].1), 6.0); // 5 + red.x(1)
}

// ---- thisLayer writes become typed scene ops (docs §8) --------------------

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

// ---- input surface (docs §6.4) --------------------------------------------

/// `input.cursorScreenPosition` is in pixels (reference d.ts), i.e. the same
/// units as `engine.screenResolution` — scripts compute
/// `screenResolution.y - cursorScreenPosition.y` for y-up deltas.
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

/// `engine.screenResolution`/`canvasSize` are real `Vec2`s (d.ts), so vector
/// methods work on them.
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
    // QuickJS surfaces integer-valued numbers as Int.
    assert_eq!(out.property_results[0].1, ScriptValue::Int(960));
}

// ---- cursor events (d.ts ScriptModule / ILayer.solid) ---------------------

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

/// enter → down → up+click over a solid layer, with the documented payloads.
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
    // Tick 1: baseline (outside bounds, button up) — transitions only, so
    // nothing may fire yet.
    let out = e.tick(cursor_frame(solid, 0.0, 0.0, false), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str(String::new()));
    // Tick 2: move inside the 50×50 box at (100,100) → enter, local x = 10.
    let out = e.tick(cursor_frame(solid, 110.0, 95.0, false), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("enter:10".into()));
    // Tick 3: press while inside → down.
    let out = e.tick(cursor_frame(solid, 110.0, 95.0, true), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("enter:10,down".into()));
    // Tick 4: release while inside → up, then click (same object as the press).
    let out = e.tick(cursor_frame(solid, 110.0, 95.0, false), vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("enter:10,down,up,click:110".into())
    );
    // Tick 5: move back out → leave.
    let out = e.tick(cursor_frame(solid, 0.0, 0.0, false), vec![]).unwrap();
    assert_eq!(
        out.property_results[0].1,
        ScriptValue::Str("enter:10,down,up,click:110,leave".into())
    );
}

/// A layer without the solid flag never triggers cursor events (d.ts:
/// "If set to true, the layer will trigger cursor events").
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

/// A frame without audio must read as silence, not as the previous tick's
/// bands (`skip_serializing_if` would otherwise leave `__host.audio` stale).
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
    // No audio this frame → zeros, not the stale 0.75.
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(0));
}

/// `getInitialLayerConfig` returns the authored values even after scripts
/// mutated the layer (d.ts IScene).
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
    // The scripted write is live in the snapshot the host feeds back…
    let mut mutated = frame();
    mutated.layers[0].alpha = Some(0.9);
    let out = e.tick(mutated, vec![]).unwrap();
    // …but the initial config still reports the authored 0.25.
    assert_eq!(out.property_results[0].1, ScriptValue::Float(0.25));
}

/// media*Changed handlers fire only for flagged categories, with the
/// documented payload shapes (thumbnail colors as Vec3).
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
        // status unchanged — its handler must stay silent.
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
    // No media this tick → nothing new fires.
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Str("pb:1,th:1".into()));
}

// ---- resizeScreen / applyGeneralSettings (d.ts ScriptModule) ---------------

/// `resizeScreen(Vec2)` fires on resolution transitions only — the initial
/// size is init()'s job.
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
    // First tick (1920×1080 default): baseline, no dispatch.
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(0));
    // Same resolution: still nothing.
    let out = e.tick(HostFrame::default(), vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(0));
    // Resolution change: dispatched with a real Vec2.
    let frame = HostFrame {
        res_x: 2560.0,
        res_y: 1440.0,
        ..Default::default()
    };
    let out = e.tick(frame, vec![]).unwrap();
    assert_eq!(out.property_results[0].1, ScriptValue::Int(2560));
}

/// `applyGeneralSettings({language})` is delivered once at load, so scripts
/// can localize their text (the changed-keys guard pattern from the d.ts).
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

// ---- timers (docs §5.4, canceller bug fixed) ------------------------------

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
    // current value fed back = 1, so no re-register; now past 100ms → fires.
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

// ---- text-layer scripts (docs §7) -----------------------------------------

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
    assert_eq!(e.layer_text(h).unwrap(), ""); // invalid handle after destroy
}

// ---- audio buffers (docs §6.1) --------------------------------------------

/// `engine.registerAudioBuffers(res).average` must read the *matching* audioN
/// reduction — the 16-band getter returns `audio16`, not the first 16 entries
/// of `audio64` (docs/scripting-api.md §6.1). Distinct fill values per
/// resolution prove the correct array is selected.
#[test]
fn register_audio_buffers_reads_matching_resolution() {
    let e = ScriptEngine::new().unwrap();
    // Sum the whole requested buffer so the count *and* the source array matter.
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
        audio16: vec![0.5; 16],  // sum 8, len 16 → 16008
        audio32: vec![0.25; 32], // sum 8, len 32 → 32008
        audio64: vec![0.1; 64],  // sum 6.4, len 64 → 64006.4
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

/// Missing audio (`None`) yields a zero-filled buffer of the requested length,
/// never a crash (V9).
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

// ---- version surface ------------------------------------------------------

#[test]
fn version_constants_exposed() {
    assert_eq!(kirie_script::API_VERSION, "2.8");
    assert_eq!(kirie_script::TRANSLATOR_VERSION, 1);
}
