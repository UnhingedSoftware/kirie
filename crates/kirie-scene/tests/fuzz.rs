use std::panic::{AssertUnwindSafe, catch_unwind};

use kirie_formats::pkg::OwnedPkg;
use kirie_scene::Scene;

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

fn synthetic_seed() -> String {
    r#"{"camera":{"center":"0 0 0","eye":"0 0 1","up":"0 1 0"},
        "general":{"ambientcolor":"0.3 0.3 0.3","bloom":false,"clearcolor":"0 0 0",
                   "orthogonalprojection":{"height":1080,"width":1920}},
        "objects":[{"id":1,"name":"o","image":"models/a.json","origin":"0 0 0",
                    "scale":"1 1 1","angles":"0 0 0","size":"100 100","visible":true,
                    "effects":[{"file":"effects/x/effect.json","id":2,"visible":true,
                                "passes":[{"id":3,"textures":[null,"t"],
                                           "constantshadervalues":{"a":1.0}}]}],
                    "animationlayers":[{"id":4,"name":"idle","animation":7,"rate":1.0,
                                        "blend":1.0,"visible":true,"additive":false}]}],
        "version":1}"#
        .to_owned()
}

fn corpus_seeds() -> Vec<String> {
    let mut out = Vec::new();
    let roots = [
        "/tank/kirie-sweep/lib/steamapps/workshop/content/431960".to_owned(),
        format!(
            "{}/.local/share/Steam/steamapps/workshop/content/431960",
            std::env::var("HOME").unwrap_or_default()
        ),
    ];
    for root in roots {
        let Ok(dirs) = std::fs::read_dir(&root) else { continue };
        for dir in dirs.flatten() {
            if out.len() >= 10 {
                return out;
            }
            let Ok(pkg) = OwnedPkg::from_path(dir.path().join("scene.pkg")) else {
                continue;
            };
            let Some(entry) = pkg.get(b"scene.json") else { continue };
            if let Ok(bytes) = pkg.read(&entry)
                && let Ok(text) = std::str::from_utf8(bytes)
            {
                out.push(text[..text.len().min(300_000)].to_owned());
            }
        }
    }
    out
}

#[test]
fn seeds_parse_so_the_fuzz_reaches_real_code() {
    assert!(Scene::from_slice(synthetic_seed().as_bytes()).is_ok(), "synthetic scene must parse");
    let corpus = corpus_seeds();
    if corpus.is_empty() {
        return;
    }
    let ok = corpus.iter().filter(|s| Scene::from_slice(s.as_bytes()).is_ok()).count();
    println!("scene seeds {} parsing {ok}", corpus.len());
    assert!(ok > 0, "no real scene parsed, the fuzz would only see rejects");
}

#[test]
fn scene_parsing_never_panics_on_mutated_input() {
    let mut seeds = vec![synthetic_seed()];
    seeds.extend(corpus_seeds());
    let rounds: u32 = std::env::var("KIRIE_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_000);

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..rounds {
        let mut bytes = seeds[rng.below(seeds.len())].clone().into_bytes();
        if bytes.is_empty() {
            continue;
        }
        match round % 4 {
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
            2 => {
                let needles: [&[u8]; 6] = [
                    b"1e400", b"-1", b"null", b"[]", b"{}", b"18446744073709551616",
                ];
                let needle = needles[rng.below(needles.len())];
                let at = rng.below(bytes.len());
                bytes.splice(at..at, needle.iter().copied());
            }
            _ => {
                let at = rng.below(bytes.len());
                let len = rng.below(8192).min(bytes.len() - at);
                bytes = bytes[at..at + len].to_vec();
            }
        }
        let payload = bytes.clone();
        let survived = catch_unwind(AssertUnwindSafe(|| {
            let _ = Scene::from_slice(&payload);
        }))
        .is_ok();
        assert!(survived, "scene parsing panicked on round {round}");
    }
}
