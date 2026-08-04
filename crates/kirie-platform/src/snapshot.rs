//! Leaving a still behind when a hidden output is released.
//!
//! `--release-hidden-after` drops a covered output's renderer to give its
//! memory back (measured on a web wallpaper: 463820KB → 220540KB, because
//! dropping the renderer also kills the out-of-process webkit host). For every
//! wallpaper the engine composites itself that is visually free: a wayland
//! surface keeps its last committed buffer for as long as the client stays
//! silent — verified by SIGSTOPping the engine, which stops it committing
//! entirely, and still grabbing the full frame seconds later — so a released
//! scene/video/image output goes on showing its final frame with no help at
//! all.
//!
//! The webview web backend breaks that, and is the only reason this module
//! exists. webkit renders into its **own** gtk-layer-shell window stacked over
//! the engine's surface, so the engine never composites a web pixel and the
//! last buffer it committed is black ([`crate::Renderer::is_passive`]).
//! Releasing kills that host, its window disappears, and the black engine
//! surface becomes what the user sees — not a flash at resume, but the whole
//! time the wallpaper stays hidden.
//!
//! [`present_still`] fixes exactly that: one frame, blitted from the still the
//! host drew for us ([`crate::Renderer::snapshot`]) and presented. Nothing is retained
//! — the texture, the pipeline and the staging copy are all gone by the time it
//! returns, and the compositor holds the resulting buffer for free. The output
//! is left showing the page, frozen, at zero ongoing cost.

use crate::renderer::RendererSnapshot;

/// Blit `still` into `surface`'s next swapchain image and present it, so that
/// frozen frame becomes the surface's last committed buffer.
///
/// Returns `true` when the frame was presented. Every failure — a malformed
/// buffer, a swapchain image that cannot be acquired, a device error — returns
/// `false`, and the caller simply carries on releasing: the whole point of a
/// stand-in is that it is a bonus, so it must never be able to hold the reclaim
/// hostage.
///
/// Owns nothing afterwards. The upload texture and the blit pipeline are
/// function-local by design (the earlier draft of this kept a ~14.7MB 1440p
/// texture resident for the entire release, which defeats the reclaim it is
/// supposed to be riding along with); once the compositor has the buffer, the
/// GPU-side copy is dead weight.
pub(crate) fn present_still(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface: &wgpu::Surface<'static>,
    still: &RendererSnapshot,
) -> bool {
    let (width, height) = (still.width, still.height);
    if width == 0 || height == 0 {
        return false;
    }
    // The bytes came from another process, so their length is a claim, not a
    // fact (SPEC §V9): check it against the dimensions before trusting either.
    let Some(expected) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
    else {
        return false;
    };
    if still.data.len() < expected {
        tracing::debug!(
            width,
            height,
            got = still.data.len(),
            expected,
            "still buffer shorter than its dimensions; releasing without one"
        );
        return false;
    }

    // Acquire before doing any GPU work, so a swapchain that cannot hand out an
    // image costs nothing. This is the one call here that could in principle
    // block: the platform's own note on passive renderers records an acquire
    // parking in `ppoll` when a surface is fully covered and the compositor
    // therefore never releases its buffers. It does not apply to this call —
    // a webview output has presented exactly ONE frame in its life (the passive
    // guard bows out of every later frame callback), so the swapchain still has
    // free images and the acquire returns immediately.
    let acquired = match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
        // Outdated/Lost/Timeout: do NOT reconfigure and retry. The output is
        // being released, the caller has a working fallback (today's black),
        // and reconfiguring a swapchain on the way out is a lot of risk for a
        // cosmetic win.
        other => {
            tracing::debug!(status = ?other, "no swapchain image for the release still");
            return false;
        }
    };
    // The live surface format, not a cached one — the pipeline's colour target
    // must match the image we are about to draw into, exactly.
    let format = acquired.texture.format();
    let view = acquired
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    let source = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-release-still"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // The still's own byte order, so a BGRA buffer (cairo `ARGB32` on
        // little-endian, which is what the webview host writes) is sampled as
        // BGRA by the GPU instead of being shuffled a pixel at a time on the
        // CPU. `*UnormSrgb` linearises on read; writing to the swapchain
        // re-encodes.
        format: still.format.wgpu_srgb(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &still.data[..expected],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            // Tight rows: the producer strips its row padding (the webview host
            // drops cairo's stride) precisely so this is the whole contract.
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-release-still-shader"),
        source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
    });
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kirie-release-still-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kirie-release-still-layout"),
        bind_group_layouts: &[Some(&bind_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-release-still-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    // Linear filtering, so a still whose size does not exactly match the
    // surface (a host widget allocation lagging a resize) stretches smoothly
    // rather than showing blocky nearest-neighbour edges.
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kirie-release-still-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kirie-release-still-bg"),
        layout: &bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("kirie-release-still-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kirie-release-still-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The triangle covers the whole viewport; the clear only
                    // matters if the draw were dropped, and black is what the
                    // surface would have shown anyway.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
    queue.present(acquired);

    // Wait for the blit to retire before returning. Not for correctness (wgpu
    // ref-counts the resources the submission still needs) but so the upload
    // texture and staging copy are genuinely gone by the time the caller runs
    // its `trim_heap`/`pageout_cold_libs` — otherwise the reclaim this is
    // riding along with would measure them as still in use.
    if let Err(err) = device.poll(wgpu::PollType::wait_indefinitely()) {
        tracing::debug!(%err, "gpu poll after presenting the release still failed");
    }
    true
}

/// Fullscreen-triangle blit of the still, UVs derived from the clip position
/// with a top-left origin (browser buffers are top-left origin).
///
/// Alpha is forced to 1: cairo's `ARGB32` is *premultiplied*, and the wallpaper
/// surface is marked fully opaque anyway, so passing the source alpha through
/// could only ever darken the still on a compositor that honoured it.
const BLIT_WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // (0,0) (2,0) (0,2) in UV space -> a triangle covering [0,1]^2.
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VsOut;
    out.uv = uv;
    out.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    // Clip-space y is up; texture/UV y is down. Flip so uv.y=0 is the top.
    out.pos.y = -out.pos.y;
    return out;
}

@group(0) @binding(0) var still_tex: texture_2d<f32>;
@group(0) @binding(1) var still_sampler: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(still_tex, still_sampler, in.uv).rgb, 1.0);
}
"#;
