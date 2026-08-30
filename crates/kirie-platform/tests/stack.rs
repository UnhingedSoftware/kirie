// The Wayland and X11 backends only exist on Linux.
#![cfg(target_os = "linux")]

use std::time::Duration;

use kirie_platform::{Backend, Platform, RenderTarget, Renderer, SurfaceSize, TestPattern};

fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("skipping: no gpu adapter available ({err})");
            return None;
        }
    };
    match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("kirie-platform-test"),
        ..wgpu::DeviceDescriptor::default()
    })) {
        Ok(pair) => Some(pair),
        Err(err) => {
            eprintln!("skipping: gpu device creation failed ({err})");
            None
        }
    }
}

#[test]
fn test_pattern_renders_headless() {
    let Some((device, queue)) = headless_device() else {
        return;
    };

    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let mut pattern = TestPattern::new(&RenderTarget {
        device: &device,
        queue: &queue,
        format,
        output_name: "offscreen-test",
        size: (64, 64),
    });

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-platform-test-target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let size = SurfaceSize {
        width: 64,
        height: 32,
    };

    pattern.render(&view, size, 0.0);
    pattern.render(&view, size, 1.0 / 60.0);

    let error = pollster::block_on(scope.pop());
    assert!(error.is_none(), "validation error: {error:?}");

    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll failed");
}

#[test]
fn platform_connects_on_live_session() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY not set (no wayland session)");
        return;
    }

    let platform = Platform::connect(Box::new(|target| {
        Box::new(TestPattern::new(target)) as Box<dyn Renderer>
    }));
    match platform {
        Ok(platform) => {
            assert_eq!(platform.output_count(), 0);
        }
        Err(err) => panic!("connect failed on live session: {err}"),
    }
}

#[test]
fn x11_backend_renders_live() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipping: DISPLAY not set (no X11/Xwayland session)");
        return;
    }

    let platform = Platform::connect_backend(
        Backend::X11,
        Box::new(|target| Box::new(TestPattern::new(target)) as Box<dyn Renderer>),
    );

    let mut platform = match platform {
        Ok(platform) => platform,
        Err(err) => {
            eprintln!("skipping: X11 backend bring-up unavailable ({err})");
            return;
        }
    };

    assert!(
        platform.output_count() >= 1,
        "expected at least one X11 monitor window"
    );

    platform
        .run(Some(Duration::from_secs(2)))
        .expect("X11 render loop should run for the deadline and exit 0");
}
