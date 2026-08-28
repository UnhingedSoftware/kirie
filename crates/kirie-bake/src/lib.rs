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

pub fn limit_malloc_arenas(n: i32) {
    // SAFETY: mallopt sets an allocator tuning knob; no pointers involved.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, n.max(1));
    }
}

pub fn resolve_vulkan_icd(selector: &str) -> Option<std::path::PathBuf> {
    let sel = selector.trim().to_ascii_lowercase();
    if sel.is_empty() || sel == "auto" {
        return None;
    }

    let explicit = std::path::Path::new(selector);
    let manifest = if explicit.is_file() {
        explicit.to_path_buf()
    } else {
        let tokens: &[&str] = match sel.as_str() {
            "amd" | "radeon" | "radv" => &["radeon", "amd"],
            "intel" | "anv" => &["intel"],
            "nvidia" => &["nvidia"],
            "nouveau" | "nvk" => &["nouveau", "nvk"],
            "software" | "lavapipe" | "llvmpipe" | "lvp" => &["lvp"],
            other => std::slice::from_ref(&other).to_owned().leak(),
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

pub fn trim_heap() {
    // SAFETY: malloc_trim(0) only releases free arena memory back to the OS;
    unsafe {
        libc::malloc_trim(0);
    }
}

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
        unsafe {
            libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_PAGEOUT);
        }
    }
}

pub fn map_readonly(path: &std::path::Path) -> std::io::Result<Box<dyn AsRef<[u8]> + Send + Sync>> {
    let f = std::fs::File::open(path)?;
    // SAFETY: read-only mapping of a file kirie never writes while mapped
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
