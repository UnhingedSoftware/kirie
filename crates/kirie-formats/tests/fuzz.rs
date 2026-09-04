use std::panic::{AssertUnwindSafe, catch_unwind};

use kirie_formats::model::{Model, PuppetMesh};
use kirie_formats::pkg::{OwnedPkg, Pkg};
use kirie_formats::project::Project;
use kirie_formats::tex::Tex;

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

fn synthetic_seeds() -> Vec<Vec<u8>> {
    let mut mdl = b"MDLV0023\0".to_vec();
    mdl.extend_from_slice(&0x0180_000Fu32.to_le_bytes());
    mdl.extend_from_slice(&1u32.to_le_bytes());
    mdl.extend_from_slice(&1u32.to_le_bytes());
    mdl.extend_from_slice(b"materials/fuzz.json\0");
    mdl.extend_from_slice(&0u32.to_le_bytes());
    mdl.extend_from_slice(&[0u8; 24]);
    mdl.extend_from_slice(&0x0180_000Fu32.to_le_bytes());
    mdl.extend_from_slice(&(80u32 * 3).to_le_bytes());
    mdl.extend_from_slice(&[7u8; 240]);
    mdl.extend_from_slice(&6u32.to_le_bytes());
    mdl.extend_from_slice(&[0u8; 6]);
    mdl.extend_from_slice(&[0u8, 0u8]);
    mdl.extend_from_slice(b"MDLS0004\0");
    mdl.extend_from_slice(&[0u8; 32]);

    let mut pkg = Vec::new();
    pkg.extend_from_slice(&8u32.to_le_bytes());
    pkg.extend_from_slice(b"PKGV0001");
    pkg.extend_from_slice(&1u32.to_le_bytes());
    pkg.extend_from_slice(&4u32.to_le_bytes());
    pkg.extend_from_slice(b"a.tx");
    pkg.extend_from_slice(&0u32.to_le_bytes());
    pkg.extend_from_slice(&4u32.to_le_bytes());
    pkg.extend_from_slice(b"data");

    let mut tex = b"TEXV0005\0".to_vec();
    tex.extend_from_slice(b"TEXI0001\0");
    tex.extend_from_slice(&[3u8; 48]);

    vec![
        mdl,
        pkg,
        tex,
        br#"{"title":"t","file":"scene.pkg","general":{"properties":{"a":{"type":"bool","value":true}}}}"#
            .to_vec(),
    ]
}

fn corpus_seeds() -> Vec<Vec<u8>> {
    let mut models = Vec::new();
    let mut texes = Vec::new();
    let roots = [
        "/tank/kirie-sweep/lib/steamapps/workshop/content/431960".to_owned(),
        format!(
            "{}/.local/share/Steam/steamapps/workshop/content/431960",
            std::env::var("HOME").unwrap_or_default()
        ),
    ];
    for root in roots {
        let Ok(dirs) = std::fs::read_dir(&root) else {
            continue;
        };
        for dir in dirs.flatten() {
            if models.len() >= 8 && texes.len() >= 8 {
                break;
            }
            let Ok(pkg) = OwnedPkg::from_path(dir.path().join("scene.pkg")) else {
                continue;
            };
            let wanted: Vec<Vec<u8>> = pkg
                .entries()
                .filter(|e| e.name.ends_with(b".mdl") || e.name.ends_with(b".tex"))
                .map(|e| e.name.to_vec())
                .collect();
            for name in wanted {
                let is_model = name.ends_with(b".mdl");
                if is_model && models.len() >= 8 {
                    continue;
                }
                if !is_model && texes.len() >= 8 {
                    continue;
                }
                let Some(entry) = pkg.get(&name) else { continue };
                let Ok(bytes) = pkg.read(&entry) else { continue };
                let payload = bytes[..bytes.len().min(200_000)].to_vec();
                if is_model {
                    models.push(payload);
                } else {
                    texes.push(payload);
                }
            }
        }
    }
    models.extend(texes);
    models
}

#[test]
fn seeds_are_parseable_so_the_fuzz_reaches_real_code() {
    let synthetic = synthetic_seeds();
    assert!(
        PuppetMesh::parse(&synthetic[0]).is_ok(),
        "synthetic mdl must parse"
    );
    assert!(Model::parse(&synthetic[0]).is_ok(), "synthetic model must parse");
    assert!(Pkg::parse(&synthetic[1]).is_ok(), "synthetic pkg must parse");
    assert!(
        Tex::parse(&synthetic[2]).is_err(),
        "synthetic tex is a header stub"
    );

    let corpus = corpus_seeds();
    if corpus.is_empty() {
        return;
    }
    let models = corpus.iter().filter(|b| Model::parse(b).is_ok()).count();
    let texes = corpus.iter().filter(|b| Tex::parse(b).is_ok()).count();
    println!("corpus seeds {} models {models} texes {texes}", corpus.len());
    assert!(
        models > 0,
        "no real model parsed, the fuzz would only see rejects"
    );
    assert!(texes > 0, "no real tex parsed, the fuzz would only see rejects");
}

#[test]
fn parsers_never_panic_on_mutated_input() {
    let mut seeds = synthetic_seeds();
    seeds.extend(corpus_seeds());
    let rounds: u32 = std::env::var("KIRIE_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_000);

    let mut rng = Rng(0x5eed_1234_9abc_def1);
    for round in 0..rounds {
        let mut data = seeds[rng.below(seeds.len())].clone();
        if data.is_empty() {
            continue;
        }
        match round % 4 {
            0 => {
                for _ in 0..1 + rng.below(8) {
                    let at = rng.below(data.len());
                    data[at] ^= 1 << rng.below(8);
                }
            }
            1 => {
                let cut = rng.below(data.len());
                data.truncate(cut);
            }
            2 => {
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(data.len().saturating_sub(4).max(1));
                    if at + 4 <= data.len() {
                        data[at..at + 4].copy_from_slice(&(rng.next() as u32).to_le_bytes());
                    }
                }
            }
            _ => {
                let at = rng.below(data.len().saturating_sub(64).max(1));
                let len = rng.below(4096).min(data.len() - at);
                data = data[at..at + len].to_vec();
            }
        }

        let payload = data.clone();
        let survived = catch_unwind(AssertUnwindSafe(|| {
            let _ = PuppetMesh::parse(&payload);
            let _ = Model::parse(&payload);
            let _ = Tex::parse(&payload);
            let _ = Pkg::parse(&payload);
            if let Ok(text) = std::str::from_utf8(&payload)
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
            {
                let _ = Project::from_value(value);
            }
        }))
        .is_ok();
        assert!(survived, "a parser panicked on round {round}: {data:?}");
    }
}
