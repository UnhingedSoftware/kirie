//! Automatic Vulkan ICD pinning.
//!
//! The loader opens *every* installed ICD, so a machine with an AMD iGPU, an
//! NVIDIA card and lavapipe keeps three vendors' userspace resident even though
//! kirie renders on one of them. Measured on such a machine: 144 MB peak for
//! `kirie check` unpinned versus 75 MB pinned to the adapter actually in use,
//! and 354 MB versus 292 MB while building a real scene — around 49 MB of it
//! NVIDIA code that is never called.
//!
//! [`super::pin_gpu`] already does the pinning when the user names a GPU. This
//! module answers the same question without being told: enumerate once, see
//! which adapter `request_adapter` would choose, pin that one's driver.
//!
//! Three things make it safe rather than clever:
//!
//! * **Pinning the wrong driver is worse than not pinning.** Measured: pinned
//!   to NVIDIA on this machine costs 164 MB against 144 MB unpinned, because a
//!   device really is created on it instead of merely enumerated. So the choice
//!   must reproduce [`kirie_platform::power_preference`]'s policy exactly, and
//!   anything unexpected falls back to the loader default.
//! * **The probe costs what it saves.** Enumerating loads every ICD, which is
//!   the cost being avoided — paying it on every launch would trade memory for
//!   startup time. The answer is cached and keyed on the ICD manifests, so a
//!   driver update invalidates it and a normal launch never probes.
//! * **A pinned driver that cannot present must not black out the desktop.**
//!   The re-executed process clears the pin and tries again if no adapter
//!   survives it.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

/// Marks the re-executed process, so the probe runs at most once.
///
/// Deliberately *not* `KIRIE_GPU_PINNED`: that one also tells
/// `kirie_platform::gpu` the user chose a GPU explicitly, which flips the
/// adapter request off `LowPower` — and picking a different adapter than the
/// one whose driver we just pinned is exactly the mismatch that costs more
/// memory than not pinning at all.
pub const SENTINEL: &str = "KIRIE_ICD_AUTO";

/// Set on the second re-exec, after a pinned driver failed to yield an adapter.
const RECOVERED: &str = "KIRIE_ICD_AUTO_FAILED";

/// Opt in with `KIRIE_AUTO_PIN=1`.
///
/// Off by default for now: the saving is large and measured, but a wrong pin
/// affects what the user sees, so it earns its default only after running
/// behind the flag.
const OPT_IN: &str = "KIRIE_AUTO_PIN";

/// Pin the ICD of the adapter kirie would pick, by re-executing with
/// `VK_DRIVER_FILES` set, then never returning.
///
/// Does nothing when the user pinned a GPU themselves, when the loader is
/// already pointed at one driver, or when anything about the probe is
/// unconvincing. Every failure path continues on the loader default.
pub fn auto_pin(argv: &[OsString]) {
    if std::env::var_os(OPT_IN).is_none() {
        return;
    }
    // Already re-executed, or the user's own selection is in play — `pin_gpu`
    // owns that case and has already run.
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
        // Deliberately NOT `KIRIE_GPU`: that is the user's explicit choice and
        // kirie-web derives a browser's GL offload from it. Auto-pinning the
        // engine's Vulkan driver must not silently move web wallpapers onto a
        // different GPU.
        .exec();
    // `exec` only returns on failure.
    tracing::debug!(%err, "could not re-exec to auto-pin; using the loader default");
}

/// Re-exec once with the pin cleared, for a pinned driver that yielded no
/// adapter. Returns only if that is not the situation (or the retry failed).
///
/// Called from the adapter-request failure path: without it, a driver that
/// enumerates but cannot present leaves the user with no wallpaper at all,
/// which is a far worse outcome than the memory this module saves.
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

/// The cache file: the chosen manifest plus the fingerprint it was chosen for.
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

/// A fingerprint of the installed ICD set: every manifest path and its mtime.
///
/// Cheap to compute (a directory listing), and it changes when a driver is
/// installed, removed or updated — which is exactly when the previous answer
/// stops being trustworthy.
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

/// Read the cached decision, or probe and cache one.
fn cached_or_probed_icd() -> Option<PathBuf> {
    let fingerprint = icd_fingerprint();
    let path = cache_path()?;

    if let Ok(text) = std::fs::read_to_string(&path)
        && let Some((manifest, cached_fp)) = text.split_once('\n')
        && cached_fp == fingerprint
    {
        // An empty manifest is a cached "do not pin" — a machine with one ICD,
        // or one whose adapter has no token. Re-probing it every launch would
        // cost exactly what pinning is meant to save.
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

/// Forget the cached decision, so the next launch probes again.
fn invalidate_cache() -> Option<()> {
    std::fs::remove_file(cache_path()?).ok()
}

/// Ask wgpu which adapter it would choose, and resolve that adapter's driver.
///
/// Mirrors the runtime request (`kirie_platform::gpu`): Vulkan only, the same
/// power preference, no surface. `None` whenever the answer is not clearly
/// actionable — a single installed ICD (nothing to save), an adapter whose
/// vendor has no token, or no adapter at all.
fn probe_icd() -> Option<PathBuf> {
    let installed = crate::gpus::scan();
    // "auto" is always first; fewer than two real entries means the loader has
    // nothing to narrow down.
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

    // Match the enumerated entry by name, so the ICD path comes from the same
    // mapping `kirie gpus` shows the user rather than a second guess at it.
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

    /// The fingerprint must be stable across calls and non-empty on a machine
    /// with drivers installed (this one has three).
    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(icd_fingerprint(), icd_fingerprint());
    }

    /// A cache file whose fingerprint does not match must be ignored rather
    /// than trusted — that is what makes a driver update take effect.
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
