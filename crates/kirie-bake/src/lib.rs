pub mod baker;
pub mod bundle;
pub mod cache;
pub mod error;
pub mod gc;
pub mod key;

pub use baker::{BackgroundBaker, BakeOutcome, BakerConfig, ContentFn, PauseFn, SourceFn, never_pause};

pub(crate) fn we_assets_shaders_dir() -> Option<std::path::PathBuf> {
    const ROOTS: [&str; 4] = [
        ".local/share/Steam/steamapps/common/wallpaper_engine/assets",
        ".steam/steam/steamapps/common/wallpaper_engine/assets",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/wallpaper_engine/assets",
        "snap/steam/common/.local/share/Steam/steamapps/common/wallpaper_engine/assets",
    ];
    if let Some(over) = std::env::var_os("KIRIE_WE_ASSETS") {
        let p = std::path::PathBuf::from(over).join("shaders");
        return p.is_dir().then_some(p);
    }
    let home = std::env::var_os("HOME")?;
    let home = std::path::PathBuf::from(home);
    ROOTS
        .iter()
        .map(|r| home.join(r).join("shaders"))
        .find(|p| p.is_dir())
}

#[cfg(target_os = "linux")]
pub fn limit_malloc_arenas(n: i32) {
    // SAFETY: mallopt sets an allocator tuning knob; no pointers involved. It
    // is safe from any thread at any time -- glibc takes its own lock -- and
    // reports failure in its return value rather than by misbehaving, so there
    // is nothing to check.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, n.max(1));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn limit_malloc_arenas(_n: i32) {}

pub fn resolve_vulkan_icd(selector: &str) -> Option<std::path::PathBuf> {
    let sel = selector.trim().to_ascii_lowercase();
    if sel.is_empty() || sel == "auto" {
        return None;
    }

    let explicit = std::path::Path::new(selector);
    let manifest = if explicit.is_file() {
        explicit.to_path_buf()
    } else {
        // The fallback used to be `to_owned().leak()`, which leaked a Vec on
        // every call that named a driver this list does not know. Binding the
        // one-element array here gives it the same lifetime as the borrowed
        // arms without leaking anything.
        let unknown = [sel.as_str()];
        let tokens: &[&str] = match sel.as_str() {
            "amd" | "radeon" | "radv" => &["radeon", "amd"],
            "intel" | "anv" => &["intel"],
            "nvidia" => &["nvidia"],
            "nouveau" | "nvk" => &["nouveau", "nvk"],
            "software" | "lavapipe" | "llvmpipe" | "lvp" => &["lvp"],
            _ => &unknown,
        };
        let dirs = [
            "/usr/share/vulkan/icd.d",
            "/usr/local/share/vulkan/icd.d",
            "/etc/vulkan/icd.d",
        ];
        dirs.iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .find(|p| {
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                tokens.iter().any(|t| name.contains(t))
            })?
    };
    Some(manifest)
}

#[cfg(target_os = "linux")]
pub fn trim_heap() {
    // SAFETY: malloc_trim(0) only releases free arena memory back to the OS;
    // it touches no allocation that is still handed out, takes no pointer from
    // us, and is safe to call from any thread at any point in the program.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn trim_heap() {}

#[cfg(target_os = "linux")]
pub fn pageout_cold_libs() {
    const COLD: &[&str] = &[
        "libnvidia-gpucomp",
        "libnvidia-rtcore",
        "libnvidia-glvkspirv",
        "libcef.so",
        "libLLVM",
        "libclang-cpp",
    ];
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return;
    };
    for line in maps.lines() {
        let Some(path) = line.split_whitespace().nth(5) else {
            continue;
        };
        if !COLD.iter().any(|c| path.contains(c)) {
            continue;
        }
        let Some((range, _)) = line.split_once(' ') else {
            continue;
        };
        let Some((a, b)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16)) else {
            continue;
        };
        if end <= start {
            continue;
        }
        // SAFETY: MADV_PAGEOUT is an eviction hint on our own mapping — it
        // writes dirty pages out and drops them, and the next read faults them
        // back in, so no allocation is invalidated and nothing observable
        // changes. `start` and `end` were just parsed out of this process's own
        // /proc/self/maps and `end > start` is checked above, so the range is a
        // live mapping of ours with a non-zero length.
        unsafe {
            libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_PAGEOUT);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn pageout_cold_libs() {}

pub fn map_readonly(path: &std::path::Path) -> std::io::Result<Box<dyn AsRef<[u8]> + Send + Sync>> {
    let f = std::fs::File::open(path)?;
    // SAFETY: read-only mapping of a file kirie never writes while mapped --
    // everything this is used for is bake output, which is replaced by writing
    // a temporary file and renaming it over the old one, so an existing
    // mapping keeps the inode it was made from. A file another program edits
    // in place underneath us is the hazard `Mmap::map` cannot rule out and
    // that this call inherits.
    let map = unsafe { memmap2::Mmap::map(&f) }?;
    Ok(Box::new(map))
}
pub use bundle::{
    BUNDLE_MAGIC, BakedBundle, BakedMip, BakedReflection, BakedShader, BakedStage, BakedTable, BakedTexture,
    BundleContent, BundleHeader,
};
pub use cache::{Cache, LoadedBundle};
pub use error::BakeError;
pub use gc::{DEFAULT_CAP_BYTES, GcReport, gc};
pub use key::{BAKE_FORMAT_VERSION, BundleKey};
