pub const BUNDLE_MAGIC: u32 = 0x4b41_424b;

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BundleHeader {
    pub magic: u32,
    pub format_version: u32,
    pub translator_version: u32,
    pub source_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub enum BakedStage {
    Vertex,
    Fragment,
}

impl From<kirie_shader::Stage> for BakedStage {
    fn from(s: kirie_shader::Stage) -> Self {
        match s {
            kirie_shader::Stage::Vertex => BakedStage::Vertex,
            kirie_shader::Stage::Fragment => BakedStage::Fragment,
        }
    }
}

impl From<BakedStage> for kirie_shader::Stage {
    fn from(s: BakedStage) -> Self {
        match s {
            BakedStage::Vertex => kirie_shader::Stage::Vertex,
            BakedStage::Fragment => kirie_shader::Stage::Fragment,
        }
    }
}

#[derive(Debug, Clone, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedShader {
    pub stage: BakedStage,
    pub name: String,
    pub spirv: Vec<u32>,
    pub glsl: String,
    pub reflection: BakedReflection,
}

impl BakedShader {
    #[must_use]
    pub fn from_translated(
        stage: kirie_shader::Stage,
        name: impl Into<String>,
        ts: &kirie_shader::TranslatedShader,
    ) -> Self {
        BakedShader {
            stage: stage.into(),
            name: name.into(),
            spirv: emit_spirv(&ts.module).unwrap_or_default(),
            glsl: ts.glsl.clone(),
            reflection: BakedReflection::from(&ts.reflection),
        }
    }
}

fn emit_spirv(module: &naga::Module) -> Option<Vec<u32>> {
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(module)
        .ok()?;
    let opts = naga::back::spv::Options::default();
    naga::back::spv::write_vec(module, &info, &opts, None).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub enum BakedParamType {
    Float,
    Int,
    Vec2,
    Vec3,
    Vec4,
}

impl From<kirie_shader::reflect::ParamType> for BakedParamType {
    fn from(t: kirie_shader::reflect::ParamType) -> Self {
        use kirie_shader::reflect::ParamType as P;
        match t {
            P::Float => BakedParamType::Float,
            P::Int => BakedParamType::Int,
            P::Vec2 => BakedParamType::Vec2,
            P::Vec3 => BakedParamType::Vec3,
            P::Vec4 => BakedParamType::Vec4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum BakedParamDefault {
    Scalar(f64),
    Vector(Vec<f32>),
}

impl From<&kirie_shader::reflect::ParamDefault> for BakedParamDefault {
    fn from(d: &kirie_shader::reflect::ParamDefault) -> Self {
        use kirie_shader::reflect::ParamDefault as D;
        match d {
            D::Scalar(v) => BakedParamDefault::Scalar(*v),
            D::Vector(v) => BakedParamDefault::Vector(v.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedParameter {
    pub name: String,
    pub material: String,
    pub ty: BakedParamType,
    pub default: Option<BakedParamDefault>,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedSampler {
    pub name: String,
    pub slot: Option<u32>,
    pub texture_binding: u32,
    pub sampler_binding: u32,
    pub default_texture: Option<String>,
    pub combo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedAttribute {
    pub name: String,
    pub location: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedCombo {
    pub name: String,
    pub value: i32,
}

#[derive(Debug, Clone, Default, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedReflection {
    pub globals_block: Vec<String>,
    pub parameters: Vec<BakedParameter>,
    pub samplers: Vec<BakedSampler>,
    pub attributes: Vec<BakedAttribute>,
    pub active_combos: Vec<BakedCombo>,
}

impl From<&kirie_shader::Reflection> for BakedReflection {
    fn from(r: &kirie_shader::Reflection) -> Self {
        BakedReflection {
            globals_block: r.globals_block.clone(),
            parameters: r
                .parameters
                .iter()
                .map(|p| BakedParameter {
                    name: p.name.clone(),
                    material: p.material.clone(),
                    ty: p.ty.into(),
                    default: p.default.as_ref().map(BakedParamDefault::from),
                })
                .collect(),
            samplers: r
                .samplers
                .iter()
                .map(|s| BakedSampler {
                    name: s.name.clone(),
                    slot: s.slot,
                    texture_binding: s.texture_binding,
                    sampler_binding: s.sampler_binding,
                    default_texture: s.default_texture.clone(),
                    combo: s.combo.clone(),
                })
                .collect(),
            attributes: r
                .attributes
                .iter()
                .map(|a| BakedAttribute {
                    name: a.name.clone(),
                    location: a.location,
                })
                .collect(),
            active_combos: r
                .active_combos
                .iter()
                .map(|(name, value)| BakedCombo {
                    name: name.clone(),
                    value: *value,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedMip {
    pub width: u32,
    pub height: u32,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedTexture {
    pub name: String,
    pub format_tag: u32,
    pub width: u32,
    pub height: u32,
    pub rgba8: bool,
    pub mips: Vec<BakedMip>,
    pub data: Vec<u8>,
}

impl BakedTexture {
    #[must_use]
    pub fn rgba8(name: impl Into<String>, width: u32, height: u32, pixels: Vec<u8>) -> Self {
        let len = pixels.len() as u64;
        BakedTexture {
            name: name.into(),
            format_tag: 0,
            width,
            height,
            rgba8: true,
            mips: vec![BakedMip {
                width,
                height,
                offset: 0,
                len,
            }],
            data: pixels,
        }
    }

    #[must_use]
    pub fn is_rgba8(&self) -> bool {
        self.rgba8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedTable {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BakedBundle {
    pub header: BundleHeader,
    pub scene_json: Vec<u8>,
    pub shaders: Vec<BakedShader>,
    pub textures: Vec<BakedTexture>,
    pub tables: Vec<BakedTable>,
}

#[derive(Debug, Clone, Default)]
pub struct BundleContent {
    pub scene_json: Vec<u8>,
    pub shaders: Vec<BakedShader>,
    pub textures: Vec<BakedTexture>,
    pub tables: Vec<BakedTable>,
}

impl BundleContent {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_scene_model(
        &mut self,
        model: &kirie_scene::SceneModel,
    ) -> Result<&mut Self, crate::BakeError> {
        self.scene_json =
            serde_json::to_vec(model).map_err(|e| crate::BakeError::Serialize(e.to_string()))?;
        Ok(self)
    }

    pub fn add_translated_shader(
        &mut self,
        stage: kirie_shader::Stage,
        name: impl Into<String>,
        ts: &kirie_shader::TranslatedShader,
    ) -> &mut Self {
        self.shaders.push(BakedShader::from_translated(stage, name, ts));
        self
    }

    pub fn add_shader(&mut self, shader: BakedShader) -> &mut Self {
        self.shaders.push(shader);
        self
    }

    pub fn add_rgba8_texture(
        &mut self,
        name: impl Into<String>,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> &mut Self {
        self.textures
            .push(BakedTexture::rgba8(name, width, height, pixels));
        self
    }

    pub fn add_texture(&mut self, texture: BakedTexture) -> &mut Self {
        self.textures.push(texture);
        self
    }

    pub fn add_table(&mut self, name: impl Into<String>, data: Vec<u8>) -> &mut Self {
        self.tables.push(BakedTable {
            name: name.into(),
            data,
        });
        self
    }

    #[must_use]
    pub(crate) fn into_bundle(self, source: &[u8]) -> BakedBundle {
        BakedBundle {
            header: BundleHeader {
                magic: BUNDLE_MAGIC,
                format_version: crate::BAKE_FORMAT_VERSION,
                translator_version: kirie_shader::TRANSLATOR_VERSION,
                source_hash: *blake3::hash(source).as_bytes(),
            },
            scene_json: self.scene_json,
            shaders: self.shaders,
            textures: self.textures,
            tables: self.tables,
        }
    }
}
