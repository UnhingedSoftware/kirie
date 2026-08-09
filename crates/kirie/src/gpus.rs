//! `kirie gpus` — the GPUs this machine can render a wallpaper on.
//!
//! A shell offering a "render GPU" setting otherwise has to infer the list
//! from the Vulkan ICD manifests on disk and put a name to each one via lspci,
//! which is both fiddly and wrong in the interesting cases: an AMD APU exposes
//! the same VRAM file a discrete card does, NVIDIA exposes none at all, and
//! lspci writes the model in two different shapes depending on vendor.
//!
//! Asking the render API instead removes the guesswork entirely. wgpu reports
//! the adapters it would actually render with, already carrying the device name
//! and whether it is integrated, discrete or software — the exact facts a user
//! is choosing between. Each is paired with the token
//! [`crate::compat`]'s `--gpu` accepts, so a picker can round-trip its own
//! selection.

use std::path::PathBuf;

use anyhow::Result;

/// One selectable render GPU.
#[derive(Debug, Clone)]
pub struct Gpu {
    /// Token to pass back as `--gpu` (`auto`, `nvidia`, `amd`, `intel`, …).
    pub value: String,
    /// Human label: the adapter name plus how it is attached.
    pub label: String,
    /// `discrete` / `integrated` / `software` / `virtual` / `other`.
    pub kind: &'static str,
    /// The Vulkan ICD `--gpu <value>` would pin, when one resolves.
    pub icd: Option<PathBuf>,
}

/// The `--gpu` token for a PCI vendor id, or `None` when the vendor has no
/// token of its own (such an adapter is still listed, just not selectable by
/// vendor).
fn vendor_token(vendor: u32, kind: &str) -> Option<&'static str> {
    // Software rasterisers report a Mesa vendor id rather than real hardware,
    // so the device type is the reliable signal for them.
    if kind == "software" {
        return Some("lvp");
    }
    match vendor {
        0x10DE => Some("nvidia"),
        0x1002 | 0x1022 => Some("amd"),
        0x8086 => Some("intel"),
        0x1AF4 => Some("virtio"),
        _ => None,
    }
}

/// Every adapter kirie could render on, "Automatic" first.
///
/// Enumerates Vulkan (kirie's own backend) and deliberately does not pin a
/// driver while doing it — the whole point is to see every installed one.
#[must_use]
pub fn scan() -> Vec<Gpu> {
    let mut gpus = vec![Gpu {
        value: "auto".to_owned(),
        label: "Automatic (no pinning)".to_owned(),
        kind: "auto",
        icd: None,
    }];

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN)) {
        let info = adapter.get_info();
        let kind = match info.device_type {
            wgpu::DeviceType::DiscreteGpu => "discrete",
            wgpu::DeviceType::IntegratedGpu => "integrated",
            wgpu::DeviceType::VirtualGpu => "virtual",
            wgpu::DeviceType::Cpu => "software",
            wgpu::DeviceType::Other => "other",
        };
        let Some(token) = vendor_token(info.vendor, kind) else {
            continue;
        };
        // One entry per vendor: `--gpu` selects a *driver*, so two adapters
        // behind the same ICD are not separately selectable.
        if gpus.iter().any(|g| g.value == token) {
            continue;
        }
        gpus.push(Gpu {
            label: format!("{} ({kind})", info.name),
            icd: kirie_bake::resolve_vulkan_icd(token),
            value: token.to_owned(),
            kind,
        });
    }
    gpus
}

/// The listing as JSON — `value`/`label` pairs a settings UI can render
/// directly, plus the resolved ICD for diagnostics.
#[must_use]
pub fn to_json(gpus: &[Gpu]) -> String {
    let values: Vec<serde_json::Value> = gpus
        .iter()
        .map(|g| {
            serde_json::json!({
                "value": g.value,
                "label": g.label,
                "kind": g.kind,
                "icd": g.icd.as_ref().map(|p| p.to_string_lossy().into_owned()),
            })
        })
        .collect();
    serde_json::Value::Array(values).to_string()
}

/// Run `kirie gpus`.
///
/// # Errors
/// Only a stdout write failure; an absent Vulkan loader simply yields the
/// `auto` entry alone.
pub fn run(json: bool) -> Result<()> {
    let gpus = scan();
    if json {
        println!("{}", to_json(&gpus));
        return Ok(());
    }
    let width = gpus.iter().map(|g| g.value.len()).max().unwrap_or(0);
    for gpu in &gpus {
        println!("{:width$}  {}", gpu.value, gpu.label, width = width);
    }
    if gpus.len() == 1 {
        println!("\nno Vulkan adapter found — kirie cannot render on this machine");
    }
    Ok(())
}
