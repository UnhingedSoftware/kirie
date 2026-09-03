use std::collections::BTreeMap;

use kirie_shader::reflect::ParamType;

use super::matrix::{IDENTITY, Mat4};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlType {
    Float,
    Vec2,
    Vec3,
    Vec4,
    Mat3,
    Mat4,
    FloatArray(usize),
}

impl GlType {
    #[must_use]
    pub fn align(self) -> usize {
        match self {
            GlType::Float => 4,
            GlType::Vec2 => 8,
            _ => 16,
        }
    }

    #[must_use]
    pub fn size(self) -> usize {
        match self {
            GlType::Float => 4,
            GlType::Vec2 => 8,
            GlType::Vec3 => 12,
            GlType::Vec4 => 16,
            GlType::Mat3 => 48,
            GlType::Mat4 => 64,
            GlType::FloatArray(n) => n * 16,
        }
    }

    #[must_use]
    pub fn from_param(ty: ParamType) -> Self {
        match ty {
            ParamType::Float | ParamType::Int => GlType::Float,
            ParamType::Vec2 => GlType::Vec2,
            ParamType::Vec3 => GlType::Vec3,
            ParamType::Vec4 => GlType::Vec4,
        }
    }
}

#[must_use]
pub fn builtin_type(name: &str) -> Option<GlType> {
    if let Some(rest) = name.strip_prefix("g_Texture") {
        if let Some(idx) = rest.strip_suffix("Resolution")
            && idx.chars().all(|c| c.is_ascii_digit())
        {
            return Some(GlType::Vec4);
        }
        if rest == "0Rotation" {
            return Some(GlType::Vec4);
        }
        if rest == "0Translation" {
            return Some(GlType::Vec2);
        }
    }
    Some(match name {
        "g_TextureReductionScale"
        | "g_Time"
        | "g_Daytime"
        | "g_Brightness"
        | "g_UserAlpha"
        | "g_Alpha"
        | "g_RefractAmount" => GlType::Float,
        "g_PointerPosition" | "g_PointerPositionLast" | "g_TexelSize" | "g_TexelSizeHalf" => GlType::Vec2,
        "g_Color"
        | "g_CompositeColor"
        | "g_LightAmbientColor"
        | "g_LightSkylightColor"
        | "g_EyePosition"
        | "g_OrientationUp"
        | "g_OrientationRight"
        | "g_OrientationForward"
        | "g_Screen"
        | "g_ViewUp"
        | "g_ViewRight" => GlType::Vec3,
        "g_Color4" | "g_RenderVar0" | "g_RenderVar1" => GlType::Vec4,
        "g_NormalModelMatrix" => GlType::Mat3,
        "g_ModelViewProjectionMatrix"
        | "g_ModelViewProjectionMatrixInverse"
        | "g_EffectModelViewProjectionMatrix"
        | "g_ModelMatrix"
        | "g_ModelMatrixInverse"
        | "g_EffectModelMatrix"
        | "g_ViewProjectionMatrix"
        | "g_EffectTextureProjectionMatrix"
        | "g_EffectTextureProjectionMatrixInverse" => GlType::Mat4,
        "g_AudioSpectrum16Left" | "g_AudioSpectrum16Right" => GlType::FloatArray(16),
        "g_AudioSpectrum32Left" | "g_AudioSpectrum32Right" => GlType::FloatArray(32),
        "g_AudioSpectrum64Left" | "g_AudioSpectrum64Right" => GlType::FloatArray(64),
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    pub ty: GlType,
    pub offset: usize,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalsLayout {
    pub members: Vec<Member>,
    pub size: usize,
}

impl GlobalsLayout {
    #[must_use]
    pub fn build(names: &[String], param_types: &BTreeMap<String, GlType>) -> Self {
        let mut members = Vec::with_capacity(names.len());
        let mut offset = 0usize;
        for name in names {
            let ty = builtin_type(name)
                .or_else(|| param_types.get(name).copied())
                .unwrap_or_else(|| {
                    tracing::debug!(uniform = %name, "unknown _WEGlobals member; assuming float");
                    GlType::Float
                });
            let align = ty.align();
            offset = offset.div_ceil(align) * align;
            members.push(Member {
                name: name.clone(),
                ty,
                offset,
            });
            offset += ty.size();
        }
        let size = offset.div_ceil(16) * 16;
        GlobalsLayout { members, size }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Builtins {
    pub time: f32,
    pub daytime: f32,
    pub brightness: f32,
    pub alpha: f32,
    pub color: [f32; 4],
    pub ambient: [f32; 3],
    pub skylight: [f32; 3],
    pub pointer: [f32; 2],
    pub pointer_last: [f32; 2],
    pub texel_size: [f32; 2],
    pub mvp: Mat4,
    pub effect_mvp: Mat4,
    pub mvp_inverse: Option<Mat4>,
    pub model: Mat4,
    pub view_projection: Mat4,
    pub eye: [f32; 3],
    pub texture0_translation: [f32; 2],
    pub texture0_rotation: [f32; 4],
    pub texture_resolution: [[f32; 4]; 8],
    pub audio16: [f32; 16],
    pub audio32: [f32; 32],
    pub audio64: [f32; 64],
}

impl Default for Builtins {
    fn default() -> Self {
        Builtins {
            time: 0.0,
            daytime: 0.0,
            brightness: 1.0,
            alpha: 1.0,
            color: [1.0, 1.0, 1.0, 1.0],
            ambient: [0.0, 0.0, 0.0],
            skylight: [0.0, 0.0, 0.0],
            pointer: [0.5, 0.5],
            pointer_last: [0.5, 0.5],
            texel_size: [0.0, 0.0],
            mvp: IDENTITY,
            effect_mvp: IDENTITY,
            mvp_inverse: None,
            model: IDENTITY,
            view_projection: IDENTITY,
            eye: [0.0, 0.0, 1000.0],
            texture0_translation: [0.0, 0.0],
            texture0_rotation: [0.0, 0.0, 0.0, 0.0],
            texture_resolution: [[0.0; 4]; 8],
            audio16: [0.0; 16],
            audio32: [0.0; 32],
            audio64: [0.0; 64],
        }
    }
}

const MAX_MEMBER_FLOATS: usize = 64;

impl Builtins {
    #[must_use]
    pub fn components_into(&self, name: &str, buf: &mut [f32]) -> Option<usize> {
        if let Some(rest) = name.strip_prefix("g_Texture")
            && let Some(idx) = rest.strip_suffix("Resolution")
            && let Ok(i) = idx.parse::<usize>()
        {
            buf[..4].copy_from_slice(&self.texture_resolution.get(i).copied().unwrap_or([0.0; 4]));
            return Some(4);
        }
        let n = match name {
            "g_Time" => set(buf, &[self.time]),
            "g_Daytime" => set(buf, &[self.daytime]),
            "g_Brightness" => set(buf, &[self.brightness]),
            "g_Alpha" | "g_UserAlpha" => set(buf, &[self.alpha]),
            "g_TextureReductionScale" => set(buf, &[1.0]),
            "g_Color" | "g_CompositeColor" => set(buf, &self.color[..3]),
            "g_Color4" => set(buf, &self.color),
            "g_LightAmbientColor" => set(buf, &self.ambient),
            "g_LightSkylightColor" => set(buf, &self.skylight),
            "g_PointerPosition" => set(buf, &self.pointer),
            "g_PointerPositionLast" => set(buf, &self.pointer_last),
            "g_TexelSize" => set(buf, &self.texel_size),
            "g_TexelSizeHalf" => set(buf, &[self.texel_size[0] * 0.5, self.texel_size[1] * 0.5]),
            "g_Screen" => {
                let w = 1.0 / self.texel_size[0];
                let h = 1.0 / self.texel_size[1];
                set(buf, &[w, h, w / h])
            }
            "g_Texture0Translation" => set(buf, &self.texture0_translation),
            "g_Texture0Rotation" => set(buf, &self.texture0_rotation),
            "g_ModelViewProjectionMatrix" => set(buf, &self.mvp),
            "g_EffectModelViewProjectionMatrix" => set(buf, &self.effect_mvp),
            "g_ModelViewProjectionMatrixInverse" => match self.mvp_inverse {
                Some(m) => set(buf, &m),
                None => set(buf, &super::matrix::inverse(&self.mvp)),
            },
            "g_ModelMatrix" | "g_EffectModelMatrix" => set(buf, &self.model),
            "g_ModelMatrixInverse" => set(buf, &super::matrix::inverse(&self.model)),
            "g_ViewProjectionMatrix" => set(buf, &self.view_projection),
            "g_EffectTextureProjectionMatrix" | "g_EffectTextureProjectionMatrixInverse" => {
                set(buf, &IDENTITY)
            }
            "g_NormalModelMatrix" => set(buf, &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            "g_OrientationUp" | "g_ViewUp" => set(buf, &[0.0, 1.0, 0.0]),
            "g_OrientationRight" | "g_ViewRight" => set(buf, &[1.0, 0.0, 0.0]),
            "g_OrientationForward" => set(buf, &[0.0, 0.0, 1.0]),
            "g_EyePosition" => set(buf, &self.eye),
            "g_RenderVar0" | "g_RenderVar1" => set(buf, &[0.0, 0.0, 0.0, 0.0]),
            "g_RefractAmount" => set(buf, &[0.05]),
            "g_AudioSpectrum16Left" | "g_AudioSpectrum16Right" => set(buf, &self.audio16),
            "g_AudioSpectrum32Left" | "g_AudioSpectrum32Right" => set(buf, &self.audio32),
            "g_AudioSpectrum64Left" | "g_AudioSpectrum64Right" => set(buf, &self.audio64),
            _ => return None,
        };
        Some(n)
    }
}

#[inline]
fn set(buf: &mut [f32], src: &[f32]) -> usize {
    buf[..src.len()].copy_from_slice(src);
    src.len()
}

pub fn pack_globals(
    out: &mut Vec<u8>,
    layout: &GlobalsLayout,
    builtins: &Builtins,
    params: &BTreeMap<String, Vec<f32>>,
) {
    out.clear();
    out.resize(layout.size, 0);
    let mut scratch = [0.0f32; MAX_MEMBER_FLOATS];
    for member in &layout.members {
        if let Some(n) = builtins.components_into(&member.name, &mut scratch) {
            write_member(out, member, &scratch[..n]);
        } else if let Some(v) = params.get(&member.name) {
            write_member(out, member, v);
        }
    }
}

fn write_member(bytes: &mut [u8], member: &Member, comps: &[f32]) {
    let put = |bytes: &mut [u8], byte_off: usize, v: f32| {
        if byte_off + 4 <= bytes.len() {
            bytes[byte_off..byte_off + 4].copy_from_slice(&v.to_le_bytes());
        }
    };
    match member.ty {
        GlType::Mat3 => {
            for col in 0..3 {
                for row in 0..3 {
                    let v = comps.get(col * 3 + row).copied().unwrap_or(0.0);
                    put(bytes, member.offset + col * 16 + row * 4, v);
                }
            }
        }
        GlType::FloatArray(n) => {
            for (i, v) in comps.iter().take(n).enumerate() {
                put(bytes, member.offset + i * 16, *v);
            }
        }
        _ => {
            for (i, v) in comps.iter().enumerate() {
                put(bytes, member.offset + i * 4, *v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn builtin_types_resolve() {
        assert_eq!(builtin_type("g_Time"), Some(GlType::Float));
        assert_eq!(builtin_type("g_TexelSize"), Some(GlType::Vec2));
        assert_eq!(builtin_type("g_Color"), Some(GlType::Vec3));
        assert_eq!(builtin_type("g_ModelViewProjectionMatrix"), Some(GlType::Mat4));
        assert_eq!(builtin_type("g_NormalModelMatrix"), Some(GlType::Mat3));
        assert_eq!(builtin_type("g_Texture3Resolution"), Some(GlType::Vec4));
        assert_eq!(builtin_type("g_Texture0Translation"), Some(GlType::Vec2));
        assert_eq!(
            builtin_type("g_AudioSpectrum64Left"),
            Some(GlType::FloatArray(64))
        );
        assert_eq!(builtin_type("g_EyePosition"), Some(GlType::Vec3));
        assert_eq!(builtin_type("g_OrientationForward"), Some(GlType::Vec3));
        assert_eq!(builtin_type("g_ViewRight"), Some(GlType::Vec3));
        assert_eq!(builtin_type("g_RenderVar0"), Some(GlType::Vec4));
        assert_eq!(builtin_type("g_RefractAmount"), Some(GlType::Float));
        assert_eq!(builtin_type("SomeMaterialParam"), None);
    }

    #[test]
    fn std140_offsets_pack_scalar_then_vec2() {
        let layout = GlobalsLayout::build(&names(&["g_Time", "g_TexelSize", "g_Color4"]), &BTreeMap::new());
        assert_eq!(layout.members[0].offset, 0);
        assert_eq!(layout.members[1].offset, 8);
        assert_eq!(layout.members[2].offset, 16);
        assert_eq!(layout.size, 32);
    }

    #[test]
    fn mat4_after_float_aligns_to_16() {
        let layout = GlobalsLayout::build(
            &names(&["g_Time", "g_ModelViewProjectionMatrix"]),
            &BTreeMap::new(),
        );
        assert_eq!(layout.members[0].offset, 0);
        assert_eq!(layout.members[1].offset, 16, "mat4 aligns to 16 after a float");
        assert_eq!(layout.size, 16 + 64);
    }

    #[test]
    fn vec3_consumes_12_but_aligns_16() {
        let layout = GlobalsLayout::build(&names(&["g_Color", "g_Time"]), &BTreeMap::new());
        assert_eq!(layout.members[0].offset, 0);
        assert_eq!(layout.members[1].offset, 12, "float fills the vec3 tail padding");
        assert_eq!(layout.size, 16);
    }

    #[test]
    fn float_array_uses_stride_16() {
        let layout = GlobalsLayout::build(&names(&["g_AudioSpectrum16Left"]), &BTreeMap::new());
        assert_eq!(layout.members[0].ty, GlType::FloatArray(16));
        assert_eq!(layout.size, 16 * 16);
    }

    #[test]
    fn material_param_type_comes_from_reflection() {
        let mut params = BTreeMap::new();
        params.insert("g_Strength".to_string(), GlType::Vec3);
        let layout = GlobalsLayout::build(&names(&["g_Strength", "g_Time"]), &params);
        assert_eq!(layout.members[0].ty, GlType::Vec3);
        assert_eq!(layout.members[1].offset, 12);
    }

    #[test]
    fn pack_writes_time_and_mvp_column_major() {
        let layout = GlobalsLayout::build(
            &names(&["g_Time", "g_ModelViewProjectionMatrix"]),
            &BTreeMap::new(),
        );
        let mut b = Builtins {
            time: 2.5,
            ..Builtins::default()
        };
        b.mvp = super::super::matrix::translation([7.0, 8.0, 9.0]);
        let mut bytes = Vec::new();
        pack_globals(&mut bytes, &layout, &b, &BTreeMap::new());
        assert_eq!(bytes.len(), 80);
        assert_eq!(f32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2.5);
        let elem = |i: usize| f32::from_le_bytes(bytes[16 + i * 4..16 + i * 4 + 4].try_into().unwrap());
        assert_eq!(elem(12), 7.0);
        assert_eq!(elem(13), 8.0);
        assert_eq!(elem(14), 9.0);
        assert_eq!(elem(15), 1.0);
    }

    #[test]
    fn audio_spectrum_packs_live_bands_left_and_right() {
        let layout = GlobalsLayout::build(
            &names(&["g_AudioSpectrum16Left", "g_AudioSpectrum16Right"]),
            &BTreeMap::new(),
        );
        let mut b = Builtins::default();
        b.audio16[0] = 0.5;
        b.audio16[15] = 0.25;
        let mut bytes = Vec::new();
        pack_globals(&mut bytes, &layout, &b, &BTreeMap::new());
        let right_off = layout.members[1].offset;
        let elem = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        assert_eq!(elem(0), 0.5, "Left band 0");
        assert_eq!(elem(15 * 16), 0.25, "Left band 15 at stride 16");
        assert_eq!(elem(right_off), 0.5, "Right band 0 mirrors Left");
        assert_eq!(elem(right_off + 15 * 16), 0.25, "Right band 15 mirrors Left");
    }

    #[test]
    fn silent_builtins_pack_zero_audio() {
        let layout = GlobalsLayout::build(&names(&["g_AudioSpectrum64Left"]), &BTreeMap::new());
        let mut bytes = Vec::new();
        pack_globals(&mut bytes, &layout, &Builtins::default(), &BTreeMap::new());
        assert!(bytes.iter().all(|&x| x == 0), "silent spectrum is all-zero (V9)");
    }

    #[test]
    fn pack_writes_mat3_with_column_padding() {
        let layout = GlobalsLayout::build(&names(&["g_NormalModelMatrix"]), &BTreeMap::new());
        let mut bytes = Vec::new();
        pack_globals(&mut bytes, &layout, &Builtins::default(), &BTreeMap::new());
        assert_eq!(bytes.len(), 48);
        let elem = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        assert_eq!(elem(0), 1.0);
        assert_eq!(elem(16 + 4), 1.0);
        assert_eq!(elem(32 + 8), 1.0);
    }
}
