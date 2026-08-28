use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use kirie_launcher::{KEY_LEN, MAGIC, TRAILER_LEN};

const ZSTD_LEVEL: i32 = 19;

const STUB: &str = "kirie-launcher";

const RUNTIME_FILES: &[&str] = &[
    "kirie",
    "kirie-cef-helper",
    "kirie-webhost",
    "libcef.so",
    "libEGL.so",
    "libGLESv2.so",
    "libvk_swiftshader.so",
    "libvulkan.so.1",
    "icudtl.dat",
    "v8_context_snapshot.bin",
    "chrome_100_percent.pak",
    "chrome_200_percent.pak",
    "resources.pak",
    "chrome-sandbox",
    "vk_swiftshader_icd.json",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: kirie-pack <build-dir> <output>");
        return ExitCode::from(2);
    }
    match pack(Path::new(&args[1]), Path::new(&args[2])) {
        Ok(info) => {
            eprintln!(
                "packed {output}: stub {stub} B + blob {blob} B (runtime stripped+compressed) → {total} B, key {key}",
                output = args[2],
                stub = info.stub_len,
                blob = info.blob_len,
                total = info.total_len,
                key = info.key,
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("kirie-pack: {err}");
            ExitCode::FAILURE
        }
    }
}

struct PackInfo {
    stub_len: usize,
    blob_len: usize,
    total_len: usize,
    key: String,
}

fn pack(build_dir: &Path, output: &Path) -> io::Result<PackInfo> {
    let stub_path = build_dir.join(STUB);
    let stub_bytes = fs::read(&stub_path)
        .map_err(|e| io::Error::other(format!("reading launcher stub {}: {e}", stub_path.display())))?;

    let stage = stage_runtime(build_dir)?;
    let _cleanup = TempDir(stage.clone());
    strip_stage(&stage);
    trim_locales(&stage)?;

    let mut encoder = zstd::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let _ = encoder.multithread(workers);
    {
        let mut builder = tar::Builder::new(&mut encoder);
        builder.follow_symlinks(false);
        builder.append_dir_all(".", &stage)?;
        builder.finish()?;
    }
    let blob = encoder.finish()?;

    let key: String = blake3::hash(&blob).to_hex().chars().take(KEY_LEN).collect();

    let mut out = File::create(output)?;
    out.write_all(&stub_bytes)?;
    out.write_all(&blob)?;
    out.write_all(MAGIC)?;
    out.write_all(&(blob.len() as u64).to_le_bytes())?;
    out.write_all(key.as_bytes())?;
    out.flush()?;
    let mut perms = fs::metadata(output)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(output, perms)?;

    Ok(PackInfo {
        stub_len: stub_bytes.len(),
        blob_len: blob.len(),
        total_len: stub_bytes.len() + blob.len() + TRAILER_LEN,
        key,
    })
}

fn stage_runtime(build_dir: &Path) -> io::Result<PathBuf> {
    let stage = std::env::temp_dir().join(format!("kirie-pack-stage.{}", std::process::id()));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    for name in RUNTIME_FILES {
        let src = build_dir.join(name);
        if !src.exists() {
            return Err(io::Error::other(format!(
                "missing runtime file {} (build web-cef + the cef helper first)",
                src.display()
            )));
        }
        fs::copy(&src, stage.join(name))?;
    }
    let locales = build_dir.join("locales");
    if !locales.is_dir() {
        return Err(io::Error::other(format!("missing {}", locales.display())));
    }
    let dst = stage.join("locales");
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(&locales)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(stage)
}

fn strip_stage(stage: &Path) {
    let Ok(entries) = fs::read_dir(stage) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_bin_or_lib =
            name == "kirie" || name == "kirie-cef-helper" || name == "kirie-webhost" || name.contains(".so");
        if !is_bin_or_lib {
            continue;
        }
        match Command::new("strip").arg("--strip-unneeded").arg(&path).status() {
            Ok(s) if s.success() => {}
            Ok(_) => eprintln!("warning: strip failed on {} (kept unstripped)", path.display()),
            Err(e) => eprintln!("warning: cannot run strip ({e}); bundle will be large"),
        }
    }
}

fn trim_locales(stage: &Path) -> io::Result<()> {
    if std::env::var("KIRIE_CEF_KEEP_LOCALES").is_ok_and(|v| v == "1") {
        return Ok(());
    }
    let locales = stage.join("locales");
    if !locales.join("en-US.pak").is_file() {
        return Ok(());
    }
    for entry in fs::read_dir(&locales)? {
        let entry = entry?;
        if entry.file_name() != "en-US.pak" && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
