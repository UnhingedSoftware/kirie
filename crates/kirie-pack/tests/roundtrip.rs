use std::io::Cursor;

use kirie_pack::{Builder, Compression, Kind, Manifest, PackError, Package, Provenance, pack_dir};

fn scene_manifest() -> Manifest {
    Manifest {
        id: "koi-pond".into(),
        title: "Koi pond".into(),
        description: "Fish, water, lanterns".into(),
        author: "someone".into(),
        kind: Kind::Scene,
        entry: "scene.kscene".into(),
        preview: Some("preview.png".into()),
        tags: vec!["nature".into()],
        mature: false,
        properties: vec![],
        min_kirie: Some("0.9.0".into()),
        provenance: Provenance::Original,
    }
}

fn build(manifest: Manifest, entries: &[(&str, Vec<u8>, Compression)]) -> Vec<u8> {
    let mut b = Builder::new(manifest);
    for (path, bytes, c) in entries {
        b.add(*path, bytes.clone(), *c).unwrap();
    }
    let mut out = Cursor::new(Vec::new());
    b.write(&mut out).unwrap();
    out.into_inner()
}

fn sample() -> Vec<u8> {
    let text = b"layer water\n".repeat(2000);
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let noise: Vec<u8> = (0..10_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    build(
        scene_manifest(),
        &[
            ("scene.kscene", text, Compression::Lz4),
            ("preview.png", noise.clone(), Compression::None),
            ("textures/koi.ktx2", noise, Compression::Lz4),
        ],
    )
}

#[test]
fn what_is_written_reads_back() {
    let bytes = sample();
    let mut p = Package::from_reader(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(p.manifest(), &scene_manifest());
    assert_eq!(p.read("scene.kscene").unwrap(), b"layer water\n".repeat(2000));
    assert_eq!(p.entry("scene.kscene").unwrap().compression, "lz4");
    // Noise does not compress, so it is stored as is even when LZ4 was asked for.
    assert_eq!(p.entry("textures/koi.ktx2").unwrap().compression, "none");
    p.verify().unwrap();

    let span = p.span("preview.png").unwrap();
    assert_eq!(span.offset % 4096, 0);
    let raw = &bytes[span.offset as usize..(span.offset + span.len) as usize];
    assert_eq!(raw, p.read("preview.png").unwrap().as_slice());
    assert!(p.span("scene.kscene").is_none());
}

#[test]
fn the_same_input_gives_the_same_bytes() {
    assert_eq!(sample(), sample());
}

#[test]
fn a_changed_byte_in_an_entry_is_caught() {
    let mut bytes = sample();
    let p = Package::from_reader(Cursor::new(bytes.clone())).unwrap();
    let at = p.span("preview.png").unwrap().offset as usize + 10;
    bytes[at] ^= 1;
    let mut p = Package::from_reader(Cursor::new(bytes)).unwrap();
    assert!(matches!(
        p.read("preview.png"),
        Err(PackError::HashMismatch { .. })
    ));
    assert!(p.verify().is_err());
}

#[test]
fn a_changed_manifest_is_caught() {
    let mut bytes = sample();
    let at = bytes.windows(8).position(|w| w == b"Koi pond").unwrap();
    bytes[at] = b'X';
    assert!(matches!(
        Package::from_reader(Cursor::new(bytes)),
        Err(PackError::Corrupt(_))
    ));
}

#[test]
fn truncated_and_foreign_files_are_refused() {
    let bytes = sample();
    for cut in [0, 10, 64, 4096, bytes.len() - 1] {
        assert!(
            Package::from_reader(Cursor::new(bytes[..cut].to_vec())).is_err(),
            "cut at {cut}"
        );
    }
    assert!(matches!(
        Package::from_reader(Cursor::new(b"PK\x03\x04 a zip file, not ours".repeat(10))),
        Err(PackError::NotAPackage)
    ));
}

#[test]
fn encrypted_or_newer_packages_say_so() {
    let mut bytes = sample();
    bytes[10] = 1;
    assert!(matches!(
        Package::from_reader(Cursor::new(bytes.clone())),
        Err(PackError::UnsupportedFlags(1))
    ));
    bytes[10] = 0;
    bytes[8] = 2;
    assert!(matches!(
        Package::from_reader(Cursor::new(bytes)),
        Err(PackError::UnsupportedVersion(2))
    ));
}

#[test]
fn the_writer_refuses_bad_packages() {
    let mut b = Builder::new(scene_manifest());
    assert!(b.add("../escape", vec![], Compression::None).is_err());
    b.add("scene.kscene", vec![1], Compression::None).unwrap();
    assert!(matches!(
        b.add("scene.kscene", vec![2], Compression::None),
        Err(PackError::DuplicatePath(_))
    ));
    // The manifest names a preview that was never added.
    assert!(matches!(
        b.write(&mut Cursor::new(Vec::new())),
        Err(PackError::BadManifest(_))
    ));
}

#[test]
fn converted_items_are_not_publishable() {
    let mut m = scene_manifest();
    m.provenance = Provenance::Converted {
        source: "wallpaper-engine".into(),
        source_id: "123".into(),
    };
    assert!(!m.provenance.publishable());
    m.provenance = Provenance::ConvertedOwn {
        source: "wallpaper-engine".into(),
        source_id: "123".into(),
    };
    assert!(m.provenance.publishable());
}

#[test]
fn a_folder_packs() {
    let dir = std::env::temp_dir().join(format!("kirie-pack-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("web/js")).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(
        dir.join("kirie.json"),
        br#"{"id":"clock","title":"Clock","kind":"web","entry":"web/index.html"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("web/index.html"), b"<canvas></canvas>").unwrap();
    std::fs::write(dir.join("web/js/clock.js"), b"tick()").unwrap();
    std::fs::write(dir.join(".git/HEAD"), b"ref").unwrap();
    std::fs::write(dir.join("clock.kpk.partial"), b"being written").unwrap();

    let mut out = Cursor::new(Vec::new());
    let summary = pack_dir(&dir, &mut out).unwrap();
    assert_eq!(summary.entries, 2);
    let mut p = Package::from_reader(Cursor::new(out.into_inner())).unwrap();
    let paths: Vec<&str> = p.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["web/index.html", "web/js/clock.js"]);
    assert_eq!(p.read("web/js/clock.js").unwrap(), b"tick()");

    std::fs::write(
        dir.join("kirie.json"),
        br#"{"id":"clock","title":"Clock","kind":"web","entry":"web/index.html","titel":"typo"}"#,
    )
    .unwrap();
    assert!(pack_dir(&dir, &mut Cursor::new(Vec::new())).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
