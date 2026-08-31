use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CORPUS_DIR: &str = "/home/aiko/.steam/steam/steamapps/workshop/content/431960";
const CORPUS_ITEM_COUNT: usize = 24;
const ITEM_1388331347_ENTRIES: usize = 44;
const KNOWN_TEX_ENTRY: &str = "materials/masks/shake_mask_bdbea347ab7838b0661ae6af118d7d24d301b354.tex";

fn corpus_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("KIRIE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(CORPUS_DIR));
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!(
            "skipping corpus test: {} not found (set KIRIE_CORPUS to override)",
            dir.display()
        );
        None
    }
}

fn run_kirie<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_kirie"))
        .args(args)
        .output()
        .expect("failed to spawn kirie")
}

#[track_caller]
fn assert_success(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kirie-cli-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("cannot create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn count_files(dir: &Path) -> usize {
    let mut count = 0;
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        if entry.file_type().expect("file type").is_dir() {
            count += count_files(&entry.path());
        } else {
            count += 1;
        }
    }
    count
}

fn sstr(s: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(s.len()).unwrap().to_le_bytes().to_vec();
    v.extend_from_slice(s);
    v
}

fn build_pkg(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut v = sstr(b"PKGV0001");
    v.extend(u32::try_from(entries.len()).unwrap().to_le_bytes());
    let mut payload = Vec::new();
    for (name, data) in entries {
        v.extend(sstr(name.as_bytes()));
        v.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
        v.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
        payload.extend_from_slice(data);
    }
    v.extend(payload);
    v
}

fn build_tex(frames: Option<&[(f32, f32, f32, f32)]>) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(b"TEXV0005\0");
    v.extend(b"TEXI0001\0");
    v.extend(0u32.to_le_bytes());
    v.extend(if frames.is_some() { 4u32 } else { 0 }.to_le_bytes());
    v.extend(4u32.to_le_bytes());
    v.extend(2u32.to_le_bytes());
    v.extend(4u32.to_le_bytes());
    v.extend(2u32.to_le_bytes());
    v.extend(0u32.to_le_bytes());
    v.extend(b"TEXB0003\0");
    v.extend(1u32.to_le_bytes());
    v.extend((-1i32).to_le_bytes());
    v.extend(1u32.to_le_bytes());
    v.extend(4u32.to_le_bytes());
    v.extend(2u32.to_le_bytes());
    v.extend(0u32.to_le_bytes());
    v.extend(32i32.to_le_bytes());
    v.extend(32i32.to_le_bytes());
    for _y in 0..2u32 {
        for x in 0..4u32 {
            v.extend(if x < 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        }
    }
    if let Some(frames) = frames {
        v.extend(b"TEXS0003\0");
        v.extend(u32::try_from(frames.len()).unwrap().to_le_bytes());
        v.extend(2u32.to_le_bytes());
        v.extend(2u32.to_le_bytes());
        for &(x, y, w1, h1) in frames {
            v.extend(0u32.to_le_bytes());
            v.extend(0.5f32.to_le_bytes());
            v.extend(x.to_le_bytes());
            v.extend(y.to_le_bytes());
            v.extend(w1.to_le_bytes());
            v.extend(0.0f32.to_le_bytes());
            v.extend(0.0f32.to_le_bytes());
            v.extend(h1.to_le_bytes());
        }
    }
    v
}

#[test]
fn bare_invocation_prints_version() {
    let out = run_kirie::<[&str; 0], &str>([]);
    let stdout = assert_success(&out, "bare kirie");
    assert!(
        stdout.starts_with("kirie "),
        "expected version line, got {stdout:?}"
    );
}

#[test]
fn info_nonexistent_path_fails() {
    let out = run_kirie(["info", "/nonexistent/kirie-no-such-path"]);
    assert!(!out.status.success(), "info on a missing path must fail");
    assert!(!out.stderr.is_empty(), "a failure must print an error message");
}

#[test]
fn extract_rejects_traversal_entry_names() {
    let tmp = TempDir::new("traversal");
    for evil in [
        "../evil.txt",
        "/abs/evil.txt",
        "a/../evil.txt",
        "..",
        ".",
        "a/..",
        "./evil.txt",
        "a//evil.txt",
        "",
        "a\\..\\evil.txt",
        "..\\evil.txt",
        "a\u{0}b",
    ] {
        let pkg_path = tmp.path().join("evil.pkg");
        std::fs::write(&pkg_path, build_pkg(&[(evil, b"pwned")])).unwrap();
        let out_dir = tmp.path().join("out");
        let out = run_kirie([
            OsStr::new("extract"),
            pkg_path.as_os_str(),
            OsStr::new("-o"),
            out_dir.as_os_str(),
        ]);
        assert!(!out.status.success(), "extraction of entry {evil:?} must fail");
        assert!(!tmp.path().join("evil.txt").exists());
        assert!(!Path::new("/abs/evil.txt").exists());
    }
}

#[test]
fn extract_rejects_non_pkg_inputs() {
    let tmp = TempDir::new("non-pkg");

    let manifest = tmp.path().join("project.json");
    std::fs::write(&manifest, br#"{"title": "x", "file": "scene.json"}"#).unwrap();
    let out = run_kirie([OsStr::new("extract"), manifest.as_os_str()]);
    assert!(!out.status.success(), "extract on a manifest must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("project.json"),
        "error should name the misdetected kind: {stderr}"
    );

    let junk = tmp.path().join("junk.bin");
    std::fs::write(&junk, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
    for cmd in ["extract", "info"] {
        let out = run_kirie([OsStr::new(cmd), junk.as_os_str()]);
        assert!(!out.status.success(), "{cmd} on junk must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot determine the type"),
            "{cmd} error message"
        );
    }
}

#[test]
fn error_chain_is_printed_without_duplicated_causes() {
    let tmp = TempDir::new("err-chain");
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, b"{\"title\": }").unwrap();
    let out = run_kirie([OsStr::new("info"), bad.as_os_str()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("expected value").count(),
        1,
        "cause repeated in: {stderr}"
    );
}

#[test]
fn extract_plain_tex_decodes_to_png() {
    let tmp = TempDir::new("plain-tex");
    let tex_path = tmp.path().join("swatch.tex");
    std::fs::write(&tex_path, build_tex(None)).unwrap();
    let out_dir = tmp.path().join("out");
    let out = run_kirie([
        OsStr::new("extract"),
        tex_path.as_os_str(),
        OsStr::new("-o"),
        out_dir.as_os_str(),
    ]);
    assert_success(&out, "extract swatch.tex");

    let png = image::open(out_dir.join("swatch.png"))
        .expect("swatch.png must decode")
        .to_rgba8();
    assert_eq!(png.dimensions(), (4, 2), "mip-0 dimensions");
    assert_eq!(png.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(png.get_pixel(3, 1).0, [0, 0, 255, 255]);
}

#[test]
fn extract_animated_tex_writes_cropped_frames() {
    let tmp = TempDir::new("anim-tex");
    let tex_path = tmp.path().join("anim.tex");
    std::fs::write(
        &tex_path,
        build_tex(Some(&[(0.0, 0.0, 2.0, 2.0), (2.0, 0.0, 2.0, 2.0)])),
    )
    .unwrap();
    let out_dir = tmp.path().join("out");
    let out = run_kirie([
        OsStr::new("extract"),
        tex_path.as_os_str(),
        OsStr::new("-o"),
        out_dir.as_os_str(),
    ]);
    assert_success(&out, "extract anim.tex");

    let frame0 = image::open(out_dir.join("anim.frame000.png"))
        .expect("frame000 must decode")
        .to_rgba8();
    let frame1 = image::open(out_dir.join("anim.frame001.png"))
        .expect("frame001 must decode")
        .to_rgba8();
    assert_eq!(frame0.dimensions(), (2, 2));
    assert_eq!(frame1.dimensions(), (2, 2));
    assert!(frame0.pixels().all(|p| p.0 == [255, 0, 0, 255]));
    assert!(frame1.pixels().all(|p| p.0 == [0, 0, 255, 255]));
}

#[test]
fn extract_rejects_out_of_bounds_frame_rect() {
    let tmp = TempDir::new("oob-frame");
    let tex_path = tmp.path().join("oob.tex");
    std::fs::write(&tex_path, build_tex(Some(&[(3.0, 0.0, 2.0, 2.0)]))).unwrap();
    let out_dir = tmp.path().join("out");
    let out = run_kirie([
        OsStr::new("extract"),
        tex_path.as_os_str(),
        OsStr::new("-o"),
        out_dir.as_os_str(),
    ]);
    assert!(
        !out.status.success(),
        "an out-of-bounds frame rect must be a hard error"
    );
}

#[test]
fn corpus_info_every_item_dir_succeeds() {
    let Some(corpus) = corpus_dir() else { return };
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&corpus)
        .expect("read corpus dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(
        dirs.len() >= CORPUS_ITEM_COUNT,
        "corpus shrank below docs/corpus.md floor: {} < {CORPUS_ITEM_COUNT}",
        dirs.len()
    );
    for dir in &dirs {
        let manifest = dir.join("project.json");
        if !manifest.is_file() {
            continue;
        }
        let readable = std::fs::read(&manifest)
            .map(|bytes| bytes.contains(&b'{'))
            .unwrap_or(false);
        if !readable {
            continue;
        }
        let out = run_kirie([OsStr::new("info"), dir.as_os_str()]);
        let stdout = assert_success(&out, &format!("info {}", dir.display()));
        assert!(
            stdout.contains("title:"),
            "info {} printed no title:\n{stdout}",
            dir.display()
        );
    }
}

#[test]
fn corpus_extract_pkg_writes_all_entries() {
    let Some(corpus) = corpus_dir() else { return };
    let pkg = corpus.join("1388331347/scene.pkg");
    if !pkg.is_file() {
        eprintln!("skipping: {} not present", pkg.display());
        return;
    }

    let out = run_kirie([OsStr::new("info"), pkg.as_os_str()]);
    let stdout = assert_success(&out, "info scene.pkg");
    assert!(
        stdout.contains(&format!("entries: {ITEM_1388331347_ENTRIES}")),
        "unexpected entry count in:\n{stdout}"
    );

    let tmp = TempDir::new("corpus-pkg");
    let out_dir = tmp.path().join("out");
    let out = run_kirie([
        OsStr::new("extract"),
        pkg.as_os_str(),
        OsStr::new("-o"),
        out_dir.as_os_str(),
    ]);
    assert_success(&out, "extract scene.pkg");

    assert_eq!(
        count_files(&out_dir),
        ITEM_1388331347_ENTRIES,
        "extracted file count vs docs/format-pkg.md §7"
    );
    let vert = std::fs::read(out_dir.join("shaders/effects/waterflow.vert"))
        .expect("waterflow.vert must be extracted");
    assert_eq!(vert.len(), 449, "waterflow.vert length vs docs/format-pkg.md §6");
    assert!(
        vert.starts_with(b"\r\nuniform mat4 g_ModelViewProje"),
        "waterflow.vert bytes vs docs/format-pkg.md §6"
    );
}

#[test]
fn corpus_extract_tex_to_png_has_expected_dimensions() {
    let Some(corpus) = corpus_dir() else { return };
    let pkg = corpus.join("1388331347/scene.pkg");
    if !pkg.is_file() {
        eprintln!("skipping: {} not present", pkg.display());
        return;
    }
    let tmp = TempDir::new("corpus-tex");

    let out_dir = tmp.path().join("pkg-out");
    let out = run_kirie([
        OsStr::new("extract"),
        pkg.as_os_str(),
        OsStr::new("--tex-to-png"),
        OsStr::new("-o"),
        out_dir.as_os_str(),
    ]);
    assert_success(&out, "extract --tex-to-png scene.pkg");
    let converted = out_dir.join(KNOWN_TEX_ENTRY).with_extension("png");
    let png = image::open(&converted)
        .unwrap_or_else(|e| panic!("{} must decode: {e}", converted.display()))
        .to_rgba8();
    assert_eq!(
        png.dimensions(),
        (1024, 1024),
        "mip-0 dimensions vs docs/format-tex.md §10.1"
    );

    let tex_path = out_dir.join(KNOWN_TEX_ENTRY);
    let info = run_kirie([OsStr::new("info"), tex_path.as_os_str()]);
    let stdout = assert_success(&info, "info on extracted tex");
    assert!(
        stdout.contains("Argb8888") && stdout.contains("1024x1024"),
        "tex info vs docs/format-tex.md §10.1:\n{stdout}"
    );

    let tex_out = tmp.path().join("tex-out");
    let out = run_kirie([
        OsStr::new("extract"),
        tex_path.as_os_str(),
        OsStr::new("-o"),
        tex_out.as_os_str(),
    ]);
    assert_success(&out, "extract standalone tex");
    let stem = Path::new(KNOWN_TEX_ENTRY).file_stem().unwrap().to_string_lossy();
    let png = image::open(tex_out.join(format!("{stem}.png")))
        .expect("standalone PNG must decode")
        .to_rgba8();
    assert_eq!(png.dimensions(), (1024, 1024));
}
