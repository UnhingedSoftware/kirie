pub const FBO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Debug)]
pub struct Fbo {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl Fbo {
    #[must_use]
    pub fn new(device: &wgpu::Device, label: &str, width: u32, height: u32) -> Self {
        Self::with_format(device, label, width, height, FBO_FORMAT)
    }

    pub fn with_format(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let ceiling = device.limits().max_texture_dimension_2d.max(1);
        let (width, height) = fit_within(width, height, ceiling);
        if width == ceiling || height == ceiling {
            tracing::debug!(
                label,
                width,
                height,
                ceiling,
                "render target clamped to the gpu limit"
            );
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Fbo {
            texture,
            view,
            width,
            height,
        }
    }
}

#[must_use]
pub fn fit_within(width: u32, height: u32, ceiling: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let ceiling = ceiling.max(1);
    if width <= ceiling && height <= ceiling {
        return (width, height);
    }
    let longest = width.max(height);
    let shrink = f64::from(ceiling) / f64::from(longest);
    let scaled = |side: u32| ((f64::from(side) * shrink).floor() as u32).clamp(1, ceiling);
    (scaled(width), scaled(height))
}

#[cfg(test)]
mod tests {
    use super::fit_within;

    #[test]
    fn a_target_inside_the_limit_is_untouched() {
        assert_eq!(fit_within(1920, 1080, 8192), (1920, 1080));
    }

    #[test]
    fn a_target_over_the_limit_keeps_its_shape() {
        assert_eq!(fit_within(8194, 4097, 8192), (8192, 4096));
    }

    #[test]
    fn a_zero_side_still_makes_a_texture() {
        assert_eq!(fit_within(0, 0, 8192), (1, 1));
    }

    #[test]
    fn a_tall_target_is_clamped_on_its_own_axis() {
        let (width, height) = fit_within(4000, 20000, 8192);
        assert_eq!(height, 8192);
        assert!(width < 4000, "{width}");
    }
}
