//! The render half of `kirie preview`: one device, many frames.
//!
//! `--screenshot` brings up a headless device, renders until the composite
//! settles, writes a PNG and exits. A preview wants the same device to stay
//! up: the cost of a frame is then the frame, not an engine start, and the
//! wallpaper animates instead of being a still.
//!
//! A property change rebuilds the renderer on the *same* device. That is what
//! the running engine does for a live `property` too, and it is fast for the
//! reason the engine's is: the scene bundle is already baked, so a rebuild
//! re-resolves the model rather than re-parsing the package.

use std::path::Path;

use anyhow::{Context, Result};
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};

use crate::compat::args::{ClampMode, ScalingMode};
use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::{Headless, build_offscreen_renderer, fit_aspect, readback, scene_projection};

/// A live, surface-less renderer for one wallpaper.
pub(crate) struct Engine {
    gpu: Headless,
    /// What is rendered into, and read back from.
    target: wgpu::Texture,
    view: wgpu::TextureView,
    size: SurfaceSize,
    renderer: Box<dyn Renderer>,
    /// The last frame read back, kept so a frame does not allocate.
    pixels: Vec<u8>,
}

/// What every preview renders as.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// One frame of wall clock, which is what the renderers are paced by.
const DT: f32 = 1.0 / 60.0;

impl Engine {
    /// Bring up a device and a wallpaper on it.
    ///
    /// # Errors
    /// When the wallpaper cannot be classified, the GPU cannot be brought up,
    /// or the renderer refuses to build.
    pub(crate) fn new(background: &Path, edge: u32) -> Result<Self> {
        let gpu = Headless::new()?;
        let wallpaper = classify(background)?;
        let size = size_for(&wallpaper, background, edge);
        let (target, view) = make_target(&gpu.device, size);
        let renderer = build(&gpu, &view_target(&gpu, size), &wallpaper, size, &[])?;
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

    /// Swap the wallpaper, the size, or the properties, keeping the device.
    ///
    /// # Errors
    /// The same as [`Engine::new`], minus the device.
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

        // Dropped before the new one is built, and it takes saying so: an
        // assignment evaluates its right-hand side first, so `self.renderer =
        // build(..)` holds two scenes' worth of textures at once — hundreds of
        // megabytes on a large scene, at the exact moment the second one is
        // allocating.
        self.renderer = build_placeholder();
        self.renderer = build(
            &self.gpu,
            &view_target(&self.gpu, size),
            &wallpaper,
            size,
            properties,
        )?;
        settle();
        Ok(())
    }

    /// Drop the scene, keep the device.
    ///
    /// A wallpaper's decoded textures are most of what a preview costs — 374 MB
    /// of the 532 MB a large scene peaks at, against a 158 MB floor for the
    /// device itself. Holding them while no one is connected is holding them
    /// for nobody. The device stays because it is what cannot be rebuilt: the
    /// driver pipeline cache attaches to the first one made in the process.
    pub(crate) fn release_scene(&mut self) {
        self.renderer = build_placeholder();
        self.pixels = Vec::new();
        settle();
    }

    /// Render one frame and read it back.
    ///
    /// # Errors
    /// When the GPU cannot be polled or the readback fails.
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

/// A wallpaper from a path, with a message a person can act on.
fn classify(background: &Path) -> Result<Wallpaper> {
    resolve::classify(&background.to_string_lossy())
        .with_context(|| format!("previewing {}", background.display()))
}

/// The size to render at: the scene's own aspect, capped to `edge`.
///
/// A preview that ignored the aspect would letterbox a portrait wallpaper into
/// a landscape frame and call it a preview.
fn size_for(wallpaper: &Wallpaper, background: &Path, edge: u32) -> SurfaceSize {
    let _ = wallpaper;
    let (width, height) = scene_projection(background).unwrap_or((16, 9));
    fit_aspect(width, height, edge)
}

/// The texture a frame is rendered into and read out of.
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

/// The render target descriptor the builders take.
fn view_target<'a>(gpu: &'a Headless, size: SurfaceSize) -> RenderTarget<'a> {
    RenderTarget {
        device: &gpu.device,
        queue: &gpu.queue,
        format: FORMAT,
        output_name: "preview",
        size: (size.width, size.height),
    }
}

/// Give back what building a scene borrowed.
///
/// Decoding a scene's textures allocates far more than the scene keeps —
/// the decode buffers are freed on the spot, but glibc holds the pages
/// against the next allocation, and a preview's next allocation may be
/// minutes away. The engine's own loop does exactly this after a build; the
/// preview path bypasses that loop and so has to say it itself.
fn settle() {
    kirie_bake::trim_heap();
    kirie_bake::pageout_cold_libs();
}

/// A renderer that owns nothing, to hold the slot while the old one is freed.
///
/// Cheaper than an `Option` around the field: every frame would otherwise have
/// to consider a state that only exists for the length of a rebuild.
fn build_placeholder() -> Box<dyn Renderer> {
    /// Draws nothing, holds nothing.
    struct Empty;
    impl Renderer for Empty {
        fn render(&mut self, _view: &wgpu::TextureView, _size: SurfaceSize, _dt: f32) {}
    }
    Box::new(Empty)
}

/// Build a renderer for the wallpaper on this device.
fn build(
    gpu: &Headless,
    target: &RenderTarget<'_>,
    wallpaper: &Wallpaper,
    size: SurfaceSize,
    properties: &[(String, String)],
) -> Result<Box<dyn Renderer>> {
    let _ = gpu;
    build_offscreen_renderer(
        target,
        wallpaper,
        ScalingMode::default(),
        ClampMode::default(),
        size,
        // No audio: a preview opens no capture device, so an audio-reactive
        // scene shows its rest state rather than taking a microphone with it.
        None,
        properties,
    )
}
