use kirie_script::{HostFrame, ScriptEngine, ScriptValue};

#[test]
fn a_runaway_script_is_interrupted_and_the_engine_survives() {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(
        "alpha_1",
        "export function update(v){ while (true) {} return 7; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();

    let start = std::time::Instant::now();
    let out = e.tick(HostFrame::default(), vec![]).expect("tick returns");
    let took = start.elapsed();
    assert!(
        took < std::time::Duration::from_secs(5),
        "a runaway script must be interrupted, took {took:?}"
    );
    assert!(
        !out.errors.is_empty(),
        "the interrupted script must report an error"
    );

    e.load_property_script(
        "alpha_2",
        "export function update(v){ return 5; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).expect("engine still ticks");
    assert!(
        out.property_results
            .iter()
            .any(|(key, value)| key == "alpha_2" && *value == ScriptValue::Int(5)),
        "a healthy script still runs after a runaway one: {:?}",
        out.property_results
    );
}

#[test]
fn a_runaway_top_level_is_interrupted_at_load() {
    let e = ScriptEngine::new().unwrap();
    let start = std::time::Instant::now();
    let _ = e.load_property_script(
        "beta_1",
        "while (true) {} export function update(v){ return v; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );

    let start = std::time::Instant::now();
    let _ = e.load_property_script(
        "beta_2",
        "function spin(){ Promise.resolve().then(spin); } spin(); export function update(v){ return v; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "an endless job chain must be cut off, took {:?}",
        start.elapsed()
    );

    e.load_property_script(
        "beta_3",
        "export function update(v){ return 5; }",
        None,
        ScriptValue::Int(0),
        serde_json::json!({}),
    )
    .unwrap();
    let out = e.tick(HostFrame::default(), vec![]).expect("engine still ticks");
    assert!(
        out.property_results
            .iter()
            .any(|(key, value)| key == "beta_3" && *value == ScriptValue::Int(5)),
        "{:?}",
        out.property_results
    );
}
