use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CORPUS_DIR: &str = "/home/aiko/.steam/steam/steamapps/workshop/content/431960";
const ITEM_TIMEOUT: Duration = Duration::from_secs(180);
const SCREENSHOT_DELAY: &str = "3";

const MODEL_ONLY_SCENES: &[&str] = &["3047596375"];

fn corpus_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("KIRIE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(CORPUS_DIR));
    dir.is_dir().then_some(dir)
}

fn have_gpu() -> bool {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).is_ok()
}

fn wait_or_kill(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
}

fn nonblack_percent(path: &std::path::Path) -> Result<f32, String> {
    let img = image::open(path)
        .map_err(|e| format!("decode {}: {e}", path.display()))?
        .to_rgba8();
    let total = img.pixels().len().max(1);
    let lit = img
        .pixels()
        .filter(|p| p.0[0] > 8 || p.0[1] > 8 || p.0[2] > 8)
        .count();
    Ok(100.0 * lit as f32 / total as f32)
}

#[test]
fn corpus_scenes_all_render_non_black() {
    let Some(dir) = corpus_dir() else {
        eprintln!("skipping corpus render gate: {CORPUS_DIR} not installed (set KIRIE_CORPUS)");
        return;
    };
    if !have_gpu() {
        eprintln!("skipping corpus render gate: no wgpu adapter");
        return;
    }

    let mut items: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file())
        .filter(|p| {
            std::fs::read(p.join("project.json"))
                .map(|bytes| bytes.contains(&b'{'))
                .unwrap_or(false)
        })
        .collect();
    items.sort();
    assert!(!items.is_empty(), "no scene.pkg items under {}", dir.display());

    let bin = env!("CARGO_BIN_EXE_kirie");
    let out_dir = std::env::temp_dir().join(format!("kirie-corpus-render-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("create scratch out dir");

    let mut failures = Vec::new();
    let mut lines = Vec::new();

    for item in &items {
        let id = item.file_name().unwrap().to_string_lossy().into_owned();
        let png = out_dir.join(format!("{id}.png"));
        let _ = std::fs::remove_file(&png);

        let spawn = Command::new(bin)
            .arg("--bg")
            .arg(item)
            .arg("--screenshot")
            .arg(&png)
            .args(["--screenshot-delay", SCREENSHOT_DELAY])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawn {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{id}: spawn failed: {e}"));
                continue;
            }
        };

        let model_only = MODEL_ONLY_SCENES.contains(&id.as_str());
        match wait_or_kill(&mut child, ITEM_TIMEOUT) {
            Some(status) if status.success() => match nonblack_percent(&png) {
                Ok(pct) if model_only => lines.push(format!(
                    "  {id:<14} OK    {pct:.1}% non-black (model-only, near-black OK)"
                )),
                Ok(pct) if pct > 5.0 => lines.push(format!("  {id:<14} OK    {pct:.1}% non-black")),
                Ok(pct) => failures.push(format!("{id}: rendered only {pct:.1}% non-black (< 5%)")),
                Err(e) => failures.push(format!("{id}: {e}")),
            },
            Some(status) => failures.push(format!("{id}: exited {status:?}")),
            None => failures.push(format!("{id}: timed out after {ITEM_TIMEOUT:?}")),
        }
    }

    let _ = std::fs::remove_dir_all(&out_dir);

    eprintln!(
        "\ncorpus render gate: {} scene items, {} ok, {} failed\n{}",
        items.len(),
        lines.len(),
        failures.len(),
        lines.join("\n")
    );
    assert!(
        failures.is_empty(),
        "scene items that did NOT render:\n  {}",
        failures.join("\n  ")
    );
}
