use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

pub const SENTINEL: &str = "KIRIE_ICD_AUTO";

const RECOVERED: &str = "KIRIE_ICD_AUTO_FAILED";

const OPT_IN: &str = "KIRIE_AUTO_PIN";

pub fn auto_pin(argv: &[OsString]) {
    if std::env::var_os(OPT_IN).is_none() {
        return;
    }
    if std::env::var_os(SENTINEL).is_some()
        || std::env::var_os("KIRIE_GPU_PINNED").is_some()
        || std::env::var_os("KIRIE_GPU").is_some()
        || std::env::var_os("VK_DRIVER_FILES").is_some()
        || super::gpu_selector(argv).is_some()
    {
        return;
    }

    let Some(manifest) = cached_or_probed_icd() else {
        return;
    };

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    tracing::debug!(icd = %manifest.display(), "auto-pinning the Vulkan driver");
    let err = std::process::Command::new(exe)
        .args(argv.iter().skip(1))
        .env("VK_DRIVER_FILES", &manifest)
        .env("VK_ICD_FILENAMES", &manifest)
        .env(SENTINEL, "1")
        .exec();
    tracing::debug!(%err, "could not re-exec to auto-pin; using the loader default");
}

pub fn recover_from_bad_pin() {
    if std::env::var_os(SENTINEL).is_none() || std::env::var_os(RECOVERED).is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    eprintln!("kirie: the auto-pinned Vulkan driver produced no adapter; retrying unpinned");
    let _ = invalidate_cache();
    let err = std::process::Command::new(exe)
        .args(args)
        .env_remove("VK_DRIVER_FILES")
        .env_remove("VK_ICD_FILENAMES")
        .env_remove(SENTINEL)
        .env(RECOVERED, "1")
        .exec();
    eprintln!("kirie: could not re-exec unpinned: {err}");
}

fn cache_path() -> Option<PathBuf> {
    let base = if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
        PathBuf::from(x).join("kirie")
    } else {
        PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
            .join(".cache")
            .join("kirie")
    };
    Some(base.join("icd-auto"))
}

fn icd_fingerprint() -> String {
    let dirs = [
        "/usr/share/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
        "/etc/vulkan/icd.d",
    ];
    let mut entries: Vec<String> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| {
            let mtime = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            format!("{}:{mtime}", e.path().display())
        })
        .collect();
    entries.sort();
    entries.join("\n")
}

fn cached_or_probed_icd() -> Option<PathBuf> {
    let fingerprint = icd_fingerprint();
    let path = cache_path()?;

    if let Ok(text) = std::fs::read_to_string(&path)
        && let Some((manifest, cached_fp)) = text.split_once('\n')
        && cached_fp == fingerprint
    {
        if manifest.is_empty() {
            return None;
        }
        let manifest = PathBuf::from(manifest);
        return manifest.is_file().then_some(manifest);
    }

    let manifest = probe_icd();
    let record = manifest
        .as_ref()
        .map_or_else(String::new, |m| m.display().to_string());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("{record}\n{fingerprint}"));
    manifest
}

fn invalidate_cache() -> Option<()> {
    std::fs::remove_file(cache_path()?).ok()
}

fn probe_icd() -> Option<PathBuf> {
    let installed = crate::gpus::scan();
    if installed.len() < 3 {
        return None;
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: kirie_platform::power_preference(),
        ..wgpu::RequestAdapterOptions::default()
    }))
    .ok()?;
    let info = adapter.get_info();

    let chosen = installed
        .iter()
        .find(|g| g.kind != "auto" && g.label.starts_with(&info.name))?;
    let icd = chosen.icd.clone()?;
    tracing::debug!(adapter = %info.name, icd = %icd.display(), "probed the auto-pin driver");
    Some(icd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(icd_fingerprint(), icd_fingerprint());
    }

    #[test]
    fn stale_fingerprint_is_not_trusted() {
        let dir = std::env::temp_dir().join("kirie-autopin-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("icd-auto");
        std::fs::write(&path, "/some/driver.json\nstale-fingerprint").expect("write");

        let text = std::fs::read_to_string(&path).expect("read");
        let (manifest, fp) = text.split_once('\n').expect("two lines");
        assert_eq!(manifest, "/some/driver.json");
        assert_ne!(fp, icd_fingerprint(), "a stale record must not match");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
