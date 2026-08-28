use kirie_scene::material::{Blending, CullMode, DepthMode};

#[must_use]
pub fn blend_state(mode: Blending) -> wgpu::BlendState {
    let component = |src, dst| wgpu::BlendComponent {
        src_factor: src,
        dst_factor: dst,
        operation: wgpu::BlendOperation::Add,
    };
    let (src, dst) = match mode {
        Blending::Normal => (wgpu::BlendFactor::One, wgpu::BlendFactor::Zero),
        Blending::Translucent => (wgpu::BlendFactor::SrcAlpha, wgpu::BlendFactor::OneMinusSrcAlpha),
        Blending::Additive => (wgpu::BlendFactor::SrcAlpha, wgpu::BlendFactor::One),
    };
    let alpha = match mode {
        Blending::Translucent => component(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha),
        _ => component(src, dst),
    };
    wgpu::BlendState {
        color: component(src, dst),
        alpha,
    }
}

#[must_use]
pub fn cull_mode(mode: CullMode) -> Option<wgpu::Face> {
    match mode {
        CullMode::Normal => Some(wgpu::Face::Back),
        CullMode::NoCull => None,
    }
}

#[must_use]
pub fn depth_stencil_state(
    depthtest: DepthMode,
    depthwrite: DepthMode,
    format: wgpu::TextureFormat,
) -> Option<wgpu::DepthStencilState> {
    match depthtest {
        DepthMode::Disabled => None,
        DepthMode::Enabled => Some(wgpu::DepthStencilState {
            format,
            depth_write_enabled: Some(matches!(depthwrite, DepthMode::Enabled)),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(src: wgpu::BlendFactor, dst: wgpu::BlendFactor) -> wgpu::BlendComponent {
        wgpu::BlendComponent {
            src_factor: src,
            dst_factor: dst,
            operation: wgpu::BlendOperation::Add,
        }
    }

    #[test]
    fn normal_is_replace() {
        let b = blend_state(Blending::Normal);
        assert_eq!(b.color, comp(wgpu::BlendFactor::One, wgpu::BlendFactor::Zero));
        assert_eq!(b.alpha, comp(wgpu::BlendFactor::One, wgpu::BlendFactor::Zero));
    }

    #[test]
    fn translucent_uses_srcalpha_oneminus() {
        let b = blend_state(Blending::Translucent);
        assert_eq!(
            b.color,
            comp(wgpu::BlendFactor::SrcAlpha, wgpu::BlendFactor::OneMinusSrcAlpha)
        );
        assert_eq!(
            b.alpha,
            comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha)
        );
    }

    #[test]
    fn additive_uses_srcalpha_one() {
        let b = blend_state(Blending::Additive);
        let expect = comp(wgpu::BlendFactor::SrcAlpha, wgpu::BlendFactor::One);
        assert_eq!(b.color, expect);
        assert_eq!(b.alpha, expect);
    }

    #[test]
    fn every_mode_maps_to_add_operation() {
        for mode in [Blending::Normal, Blending::Translucent, Blending::Additive] {
            let b = blend_state(mode);
            assert_eq!(b.color.operation, wgpu::BlendOperation::Add);
            assert_eq!(b.alpha.operation, wgpu::BlendOperation::Add);
        }
    }

    #[test]
    fn cull_mapping() {
        assert_eq!(cull_mode(CullMode::NoCull), None);
        assert_eq!(cull_mode(CullMode::Normal), Some(wgpu::Face::Back));
    }

    #[test]
    fn depth_disabled_has_no_state() {
        assert!(
            depth_stencil_state(
                DepthMode::Disabled,
                DepthMode::Disabled,
                wgpu::TextureFormat::Depth24Plus
            )
            .is_none()
        );
        assert!(
            depth_stencil_state(
                DepthMode::Disabled,
                DepthMode::Enabled,
                wgpu::TextureFormat::Depth24Plus
            )
            .is_none()
        );
    }

    #[test]
    fn depth_enabled_is_lequal_with_write_flag() {
        let fmt = wgpu::TextureFormat::Depth24Plus;
        let on = depth_stencil_state(DepthMode::Enabled, DepthMode::Enabled, fmt).unwrap();
        assert_eq!(on.depth_compare, Some(wgpu::CompareFunction::LessEqual));
        assert_eq!(on.depth_write_enabled, Some(true));

        let off = depth_stencil_state(DepthMode::Enabled, DepthMode::Disabled, fmt).unwrap();
        assert_eq!(off.depth_compare, Some(wgpu::CompareFunction::LessEqual));
        assert_eq!(off.depth_write_enabled, Some(false));
    }
}
