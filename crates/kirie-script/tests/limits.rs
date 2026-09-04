use kirie_script::{HostFrame, ScriptEngine, ScriptValue};

fn tick_with(name: &str, body: &str) -> (std::time::Duration, usize) {
    let e = ScriptEngine::new().unwrap();
    e.load_property_script(name, body, None, ScriptValue::Int(0), serde_json::json!({}))
        .unwrap();
    let start = std::time::Instant::now();
    let out = e.tick(HostFrame::default(), vec![]).expect("tick returns");
    (start.elapsed(), out.errors.len())
}

#[test]
fn runaway_recursion_throws_instead_of_smashing_the_stack() {
    let (took, errors) = tick_with(
        "alpha_1",
        "function f(n){ return n <= 0 ? 0 : 1 + f(n - 1); } export function update(v){ return f(10000000); }",
    );
    assert!(errors > 0, "unbounded recursion must surface as a script error");
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
}

#[test]
fn runaway_allocation_hits_the_heap_limit() {
    let (took, errors) = tick_with(
        "alpha_2",
        "export function update(v){ var a = []; while (true) { a.push(new Array(10000).fill(1)); } return 1; }",
    );
    assert!(errors > 0, "unbounded allocation must surface as a script error");
    assert!(
        took < std::time::Duration::from_millis(900),
        "the heap limit should stop it before the 1 s time budget, took {took:?}"
    );
}
