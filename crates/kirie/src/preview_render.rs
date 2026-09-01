use std::path::Path;

use anyhow::{Context, Result};
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};

use crate::compat::args::{ClampMode, ScalingMode};
use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::{Headless, build_offscreen_renderer, fit_aspect, readback, scene_projection};

pub(crate) struct Engine {
    gpu: Headless,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    size: SurfaceSize,
    renderer: Box<dyn Renderer>,
    pixels: Vec<u8>,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const DT: f32 = 1.0 / 60.0;

impl Engine {
    pub(crate) fn new(background: &Path, edge: u32) -> Result<Self> {
        let gpu = Headless::new()?;
        let wallpaper = classify(background)?;
        let size = size_for(&wallpaper, background, edge);
        let (target, view) = make_target(&gpu.device, size);
        let renderer = build(&gpu, &view_target(&gpu, size), &wallpaper, &[])?;
        settle();

        Ok(Self {
            pixels: Vec::with_capacity((size.width * size.height * 4) as usize),
            gpu,
            target,
            view,
            size,
            renderer,
        })
    }

    pub(crate) fn rebuild(
        &mut self,
        background: &Path,
        edge: u32,
        properties: &[(String, String)],
    ) -> Result<()> {
        let wallpaper = classify(background)?;
        let size = size_for(&wallpaper, background, edge);

        if size != self.size {
            let (target, view) = make_target(&self.gpu.device, size);
            self.target = target;
            self.view = view;
            self.size = size;
            self.pixels = Vec::with_capacity((size.width * size.height * 4) as usize);
        }

        self.renderer = build_placeholder();
        self.renderer = build(
            &self.gpu,
            &view_target(&self.gpu, size),
            &wallpaper,
            properties,
        )?;
        settle();
        Ok(())
    }

    pub(crate) fn release_scene(&mut self) {
        self.renderer = build_placeholder();
        self.pixels = Vec::new();
        settle();
    }

    pub(crate) fn frame(&mut self) -> Result<(u32, u32, &[u8])> {
        self.renderer.render(&self.view, self.size, DT);
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| anyhow::anyhow!("gpu poll after render: {error}"))?;

        self.pixels = readback(&self.gpu.device, &self.gpu.queue, &self.target, self.size)?;
        Ok((self.size.width, self.size.height, &self.pixels))
    }
}

fn classify(background: &Path) -> Result<Wallpaper> {
    let wallpaper = resolve::classify(&background.to_string_lossy())
        .with_context(|| format!("previewing {}", background.display()))?;
    if let Some(note) = resolve::refuse_without_assets(&wallpaper) {
        anyhow::bail!(note);
    }
    Ok(wallpaper)
}

fn size_for(wallpaper: &Wallpaper, background: &Path, edge: u32) -> SurfaceSize {
    let _ = wallpaper;
    let (width, height) = scene_projection(background).unwrap_or((16, 9));
    fit_aspect(width, height, edge)
}

fn make_target(device: &wgpu::Device, size: SurfaceSize) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-preview-target"),
        size: wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn view_target<'a>(gpu: &'a Headless, size: SurfaceSize) -> RenderTarget<'a> {
    RenderTarget {
        device: &gpu.device,
        queue: &gpu.queue,
        format: FORMAT,
        output_name: "preview",
        size: (size.width, size.height),
    }
}

fn settle() {
    kirie_bake::trim_heap();
    kirie_bake::pageout_cold_libs();
}

fn build_placeholder() -> Box<dyn Renderer> {
    struct Empty;
    impl Renderer for Empty {
        fn render(&mut self, _view: &wgpu::TextureView, _size: SurfaceSize, _dt: f32) {}
    }
    Box::new(Empty)
}

fn build(
    gpu: &Headless,
    target: &RenderTarget<'_>,
    wallpaper: &Wallpaper,
    properties: &[(String, String)],
) -> Result<Box<dyn Renderer>> {
    let _ = gpu;
    build_offscreen_renderer(
        target,
        wallpaper,
        ScalingMode::default(),
        ClampMode::default(),
        None,
        properties,
    )
}
