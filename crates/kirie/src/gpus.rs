use std::path::PathBuf;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Gpu {
    pub value: String,
    pub label: String,
    pub kind: &'static str,
    pub icd: Option<PathBuf>,
}

fn vendor_token(vendor: u32, kind: &str) -> Option<&'static str> {
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
