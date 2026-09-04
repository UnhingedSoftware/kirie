use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use kirie_formats::pkg::OwnedPkg;
use kirie_shader::{MapIncludeResolver, ShaderInputs, Stage, translate};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const STOCK: &str = "/home/aiko/.local/share/Steam/steamapps/common/wallpaper_engine/assets/shaders";

fn stock_seeds() -> (Vec<(Stage, String)>, BTreeMap<String, String>) {
    let mut seeds = Vec::new();
    let mut headers = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(STOCK) else {
        return (seeds, headers);
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if name.ends_with(".h") {
            headers.insert(name.to_owned(), text);
        } else if name.ends_with(".vert") && seeds.len() < 12 {
            seeds.push((Stage::Vertex, text));
        } else if name.ends_with(".frag") && seeds.len() < 24 {
            seeds.push((Stage::Fragment, text));
        }
    }
    if let Ok(base) = std::fs::read_dir(format!("{STOCK}/base")) {
        for entry in base.flatten() {
            let path = entry.path();
            let (Some(name), Ok(text)) = (
                path.file_name().and_then(|n| n.to_str()),
                std::fs::read_to_string(&path),
            ) else {
                continue;
            };
            if name.ends_with(".h") {
                headers.insert(format!("base/{name}"), text);
            }
        }
    }
    (seeds, headers)
}

fn workshop_seeds() -> Vec<(Stage, String)> {
    let mut out = Vec::new();
    let root = "/tank/kirie-sweep/lib/steamapps/workshop/content/431960";
    let Ok(dirs) = std::fs::read_dir(root) else {
        return out;
    };
    for dir in dirs.flatten() {
        if out.len() >= 12 {
            break;
        }
        let Ok(pkg) = OwnedPkg::from_path(dir.path().join("scene.pkg")) else {
            continue;
        };
        let names: Vec<Vec<u8>> = pkg
            .entries()
            .filter(|e| e.name.ends_with(b".vert") || e.name.ends_with(b".frag"))
            .map(|e| e.name.to_vec())
            .take(2)
            .collect();
        for name in names {
            let stage = if name.ends_with(b".vert") {
                Stage::Vertex
            } else {
                Stage::Fragment
            };
            let Some(entry) = pkg.get(&name) else { continue };
            if let Ok(bytes) = pkg.read(&entry)
                && let Ok(text) = std::str::from_utf8(bytes)
            {
                out.push((stage, text.to_owned()));
            }
        }
    }
    out
}

fn inputs() -> ShaderInputs {
    ShaderInputs {
        combos: BTreeMap::new(),
        override_combos: BTreeMap::new(),
        populated_texture_slots: BTreeSet::from([0, 1, 2]),
    }
}

#[test]
fn a_nul_byte_in_a_shader_is_an_error_not_a_crash() {
    let source = "#version 450\nvoid main() {\0}\n";
    let resolver = MapIncludeResolver {
        headers: BTreeMap::new(),
        fallback: None,
    };
    let out = translate(Stage::Fragment, "nul.frag", source, &resolver, &inputs());
    assert!(out.is_err(), "a NUL-bearing shader must be rejected, not compiled");
}

#[test]
fn translator_never_panics_on_mutated_shaders() {
    let cache = std::env::temp_dir().join(format!("kirie-shader-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&cache).expect("cache dir");
    kirie_shader::translate::set_cache_dir(Some(cache.clone()));

    let (mut seeds, headers) = stock_seeds();
    seeds.extend(workshop_seeds());
    if seeds.is_empty() {
        eprintln!("no shader seeds available; skipping");
        return;
    }
    let clean = seeds
        .iter()
        .filter(|(stage, src)| {
            translate(
                *stage,
                "seed",
                src,
                &MapIncludeResolver { headers: headers.clone(), fallback: None },
                &inputs(),
            )
            .is_ok()
        })
        .count();
    println!("shader seeds {} translating cleanly {clean}", seeds.len());
    assert!(clean > 0, "no seed translated, the fuzz would only see rejects");

    let rounds: u32 = std::env::var("KIRIE_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for round in 0..rounds {
        let (stage, base) = &seeds[rng.below(seeds.len())];
        let mut bytes = base.clone().into_bytes();
        if bytes.is_empty() {
            continue;
        }
        match round % 3 {
            0 => {
                for _ in 0..1 + rng.below(6) {
                    let at = rng.below(bytes.len());
                    bytes[at] = (rng.next() % 128) as u8;
                }
            }
            1 => {
                let cut = rng.below(bytes.len());
                bytes.truncate(cut);
            }
            _ => {
                let at = rng.below(bytes.len());
                let len = rng.below(2048).min(bytes.len() - at);
                bytes = bytes[at..at + len].to_vec();
            }
        }
        let source = String::from_utf8_lossy(&bytes).into_owned();
        let resolver = MapIncludeResolver {
            headers: headers.clone(),
            fallback: None,
        };
        let stage = *stage;
        let survived = catch_unwind(AssertUnwindSafe(|| {
            let _ = translate(stage, "fuzz", &source, &resolver, &inputs());
        }))
        .is_ok();
        assert!(survived, "translator panicked on round {round}");
    }
    let _ = std::fs::remove_dir_all(&cache);
}
