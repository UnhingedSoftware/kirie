//! `kirie-bake` — the hash-keyed prebaked scene-bundle cache (SPEC.md §V7/§V8).
//!
//! A *bundle* is an [`rkyv`] archive of everything the renderer needs to start a
//! scene without re-parsing, re-translating shaders, or re-decoding textures:
//! the resolved [`kirie_scene::SceneModel`], translated shader units (SPIR-V +
//! reflection), GPU-ready textures, and precomputed tables (see [`bundle`]).
//! Bundles are content-addressed by a [`BundleKey`] = `blake3(source) ⊕
//! bake-format-version ⊕ shader-translator-version` (SPEC.md §V8) and live at
//! `~/.cache/kirie/bundles/<blake3-hex>/`.
//!
//! ## Load path (warm start)
//!
//! [`Cache::load`] mmaps the bundle and validates it (blake3 checksum + rkyv
//! bytecheck), then hands back a [`LoadedBundle`] whose fields are read
//! zero-copy from the mapping. A key mismatch is a clean miss → rebake; a corrupt
//! bundle is a typed error, never a panic (SPEC.md §V9).
//!
//! ## Bake path (cold / background)
//!
//! [`Cache::bake`] writes a [`BundleContent`] atomically. The
//! [`BackgroundBaker`] watches a workshop directory and bakes new/stale items on
//! an idle-priority pool that pauses under a fullscreen app (SPEC.md §V7), with
//! LRU [`gc`] keeping the cache under a size cap.
//!
//! ## §V2 note
//!
//! The task orders `memmap2` zero-copy loading; mmap requires one `unsafe` call,
//! so this crate cannot `#![forbid(unsafe_code)]`. The two `unsafe` blocks (map +
//! post-validation `access_unchecked`) are documented with `// SAFETY:` in
//! [`cache`]. SPEC.md §V2's exception list should be extended to include
//! kirie-bake; that is a spec-owner amendment, flagged in the task report.

pub mod baker;
pub mod bundle;
pub mod cache;
pub mod error;
pub mod gc;
pub mod key;

pub use baker::{BackgroundBaker, BakeOutcome, BakerConfig, ContentFn, PauseFn, SourceFn, never_pause};

/// Cap glibc's malloc arena count (`mallopt(M_ARENA_MAX, n)`).
///
/// The installed Wallpaper Engine `assets/shaders` directory, when present.
///
/// Mirrors the engine's Steam-root probe (kept tiny and duplicated here on
/// purpose: kirie-bake stays dependency-light, and these four spellings are
/// the stable Steam install layouts). Used by [`key::BundleKey`]'s assets
/// fingerprint — see there for why bundles must track asset updates.
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

/// Every wallpaper build runs on a fresh worker thread; glibc hands each new
/// thread a (possibly new) arena, up to 8×cores — and [`trim_heap`] only
/// reliably releases the main arena. Capping the count early keeps transient
/// build allocations in a couple of arenas the trims actually reach, so RSS
/// doesn't ratchet up across wallpaper switches. Call once at startup.
pub fn limit_malloc_arenas(n: i32) {
    // SAFETY: mallopt sets an allocator tuning knob; no pointers involved.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, n.max(1));
    }
}

/// Restrict the Vulkan loader to one vendor's driver (`VK_DRIVER_FILES`).
///
/// The loader opens **every** installed ICD when an instance is created, so a
/// machine with two GPUs pays for both userspace stacks even though kirie
/// renders on one: measured on a Ryzen iGPU + RTX 4080 box, a scene wallpaper
/// is ~240MB RSS with both loaded and ~70-98MB pinned to one — the single
/// biggest memory item, dwarfing anything inside kirie (~31MB of binary +
/// heap). Pinning also makes adapter choice deterministic instead of relying
/// on wgpu's preference order.
///
/// `selector` is a vendor token (`nvidia`, `amd`/`radeon`, `intel`, `nouveau`,
/// `lvp`/`software`) matched against the ICD manifest file names, or an
/// explicit path to a manifest. `auto`/empty is a no-op (loader default).
/// Returns the manifest that was pinned, or `None` when nothing matched — a
/// miss is never fatal, it just leaves the loader's default behavior.
///
/// Setting the variables in-process is NOT enough — measured: the loader still
/// opened both stacks (~142MB of driver pages) and picked the same adapter,
/// while the identical variables set by the *parent* gave ~29MB. So the caller
/// applies this by re-executing itself with the returned manifest in the
/// environment (`kirie::compat::run`), which is the only form the loader
/// honors.
pub fn resolve_vulkan_icd(selector: &str) -> Option<std::path::PathBuf> {
    let sel = selector.trim().to_ascii_lowercase();
    if sel.is_empty() || sel == "auto" {
        return None;
    }

    // An explicit manifest path wins — lets a user point at any driver.
    let explicit = std::path::Path::new(selector);
    let manifest = if explicit.is_file() {
        explicit.to_path_buf()
    } else {
        // ICD manifests are named after their driver: nvidia_icd.json,
        // radeon_icd.x86_64.json, intel_icd.x86_64.json, lvp_icd.*.json.
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

/// Return freed heap pages to the kernel (`malloc_trim(0)`).
///
/// Wallpaper builds allocate large transient buffers (texture decode, shader
/// translation, scene JSON) that glibc's arenas retain after free — tens of
/// MB of dead RSS per build. Callers invoke this once after a build/swap
/// completes; it is a no-op-safe hint, never required for correctness. Lives
/// here for the same §V2 reason as [`map_readonly`] (the one crate allowed
/// `unsafe`; `malloc_trim` is a foreign call).
pub fn trim_heap() {
    // SAFETY: malloc_trim(0) only releases free arena memory back to the OS;
    // it takes no pointers and cannot invalidate live allocations.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Evict cold library pages from RSS (`madvise(MADV_PAGEOUT)`).
///
/// The NVIDIA Vulkan userspace keeps ~100MB of library code resident that is
/// only needed in bursts: `libnvidia-gpucomp` (shader compiler — used only
/// while building a wallpaper's pipelines), `libnvidia-rtcore` (raytracing —
/// never used), the SPIR-V compiler, and `libcef` (idle unless a web
/// wallpaper is showing). Mesa is the same shape on the AMD/Intel side:
/// `libLLVM` is RADV's shader compiler (~24MB resident, measured) and, like
/// `libnvidia-gpucomp`, is touched only while compiling pipelines — the
/// dominant non-NVIDIA entry, since a machine with both GPUs loads BOTH
/// vendors' stacks (the Vulkan loader enumerates every installed ICD unless
/// `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` pins one). After a build settles, page them out: the kernel
/// reclaims the clean file-backed pages immediately instead of "eventually",
/// and they refault transparently from the page cache/disk on next use (the
/// next wallpaper build — already a >100ms operation). Dirty pages are left
/// alone by the kernel where not swappable; correctness is unaffected either
/// way — this is purely an RSS/reclaim hint.
pub fn pageout_cold_libs() {
    const COLD: &[&str] = &[
        "libnvidia-gpucomp",
        "libnvidia-rtcore",
        "libnvidia-glvkspirv",
        "libcef.so",
        // Mesa (RADV/ANV) shader compilation: cold once pipelines are built.
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
        // never unmaps or alters contents; evicted pages refault on access.
        unsafe {
            libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_PAGEOUT);
        }
    }
}

/// Memory-map a file read-only, boxed as opaque bytes.
///
/// This lives here because kirie-bake is the one crate with the §V2 `unsafe`
/// exception for `memmap2` (see the module docs) — `forbid(unsafe_code)`
/// callers (kirie-formats/kirie-render) use it to back large read-only inputs
/// (a multi-hundred-MB `scene.pkg`) with the page cache instead of a heap
/// `Vec`, so the bytes are evictable and never counted as process RSS.
///
/// SAFETY of the map itself: the file is opened read-only and the mapping is
/// private; kirie treats workshop content as immutable while an engine runs —
/// the same assumption the bundle/shader caches already make. An external
/// truncation during use would fault, exactly like the cache mmaps above.
pub fn map_readonly(path: &std::path::Path) -> std::io::Result<Box<dyn AsRef<[u8]> + Send + Sync>> {
    let f = std::fs::File::open(path)?;
    // SAFETY: read-only mapping of a file kirie never writes while mapped
    // (workshop content is immutable per the crate contract above).
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
