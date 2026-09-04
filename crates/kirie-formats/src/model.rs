use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const VERTEX_STRIDE: usize = 48;
const MODEL_DEFAULT_FLAGS: u32 = 0xF;

pub const POSITION_OFFSET: usize = 0;
pub const NORMAL_OFFSET: usize = 12;
pub const TANGENT_OFFSET: usize = 24;
pub const UV_OFFSET: usize = 40;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error(
        "truncated model: need {needed} byte(s) for {what} at offset {offset}, only {available} available"
    )]
    UnexpectedEof {
        what: &'static str,
        offset: usize,
        needed: usize,
        available: usize,
    },

    #[error("unsupported model header {header:?} (expected \"MDLV\" prefix)")]
    BadMagic { header: String },

    #[error("invalid mesh count {count} (must be >= 0)")]
    InvalidMeshCount { count: i32 },

    #[error("mesh {index}: invalid vertex block: {vertex_bytes} byte(s) (must be > 0, a multiple of {stride}, and in bounds)", stride = VERTEX_STRIDE)]
    InvalidVertexBlock { index: usize, vertex_bytes: i32 },

    #[error("mesh {index}: invalid index block: {index_bytes} byte(s) (must be > 0, even, and in bounds)")]
    InvalidIndexBlock { index: usize, index_bytes: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 4],
    pub uv: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    pub material_ref: String,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub flags: i32,
    pub vertex_data: Vec<u8>,
    pub indices: Vec<u16>,
}

impl Mesh {
    fn layout(&self) -> VertexLayout {
        let layout = VertexLayout::for_flags(self.flags as u32);
        if layout.stride == 0 {
            VertexLayout::for_flags(MODEL_DEFAULT_FLAGS)
        } else {
            layout
        }
    }

    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertex_data.len() / self.layout().stride
    }

    pub fn vertices(&self) -> impl Iterator<Item = Vertex> + '_ {
        let layout = self.layout();
        self.vertex_data
            .chunks_exact(layout.stride)
            .map(move |chunk| decode_vertex(chunk, layout))
    }

    #[must_use]
    pub fn material_ref(&self) -> &str {
        &self.material_ref
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub version: String,
    pub header0: i32,
    pub header1: i32,
    pub meshes: Vec<Mesh>,
}

impl Model {
    pub fn parse(data: &[u8]) -> Result<Self, ModelError> {
        let mut cur = Cursor::new(data);

        let version = cur.read_cstring("version")?;
        if !version.starts_with("MDLV") {
            return Err(ModelError::BadMagic { header: version });
        }
        let number = puppet_version_number(&version).unwrap_or(u32::MAX);

        let header_flags = cur.read_i32("header0")?;
        let material_count = cur.read_i32("header1")?;
        let mesh_count = cur.read_i32("meshCount")?;
        if mesh_count < 0 {
            return Err(ModelError::InvalidMeshCount { count: mesh_count });
        }

        let mut meshes = Vec::new();
        for index in 0..mesh_count as usize {
            let mut material_ref = String::new();
            for _ in 0..material_count.max(0) {
                material_ref = cur.read_cstring("materialRef")?;
            }
            if number >= 4 && cur.read_i32("mesh reserved word")? & 2 != 0 {
                cur.read_i32("mesh reserved extra")?;
            }
            let bbox = if number >= 17 {
                cur.read_f32x6("bbox")?
            } else {
                [0.0; 6]
            };
            let flags = if number > 14 {
                cur.read_i32("flags")?
            } else {
                header_flags
            };
            let stride = VertexLayout::for_flags(flags as u32).stride;

            let vertex_bytes = cur.read_i32("vertexBytes")?;
            if vertex_bytes <= 0
                || stride == 0
                || !(vertex_bytes as usize).is_multiple_of(stride)
                || cur.remaining() < vertex_bytes as usize
            {
                return Err(ModelError::InvalidVertexBlock { index, vertex_bytes });
            }
            let vertex_data = cur.take(vertex_bytes as usize).to_vec();

            let index_bytes = cur.read_i32("indexBytes")?;
            if index_bytes <= 0 || (index_bytes % 2) != 0 || cur.remaining() < index_bytes as usize {
                return Err(ModelError::InvalidIndexBlock { index, index_bytes });
            }
            let index_slice = cur.take(index_bytes as usize);
            let indices = index_slice
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[lo, hi]| u16::from_le_bytes([lo, hi]))
                .collect();

            if number >= 21 {
                skip_mesh_trailers(&mut cur)?;
            }

            meshes.push(Mesh {
                material_ref,
                bbox_min: [bbox[0], bbox[1], bbox[2]],
                bbox_max: [bbox[3], bbox[4], bbox[5]],
                flags,
                vertex_data,
                indices,
            });
        }

        Ok(Self {
            version,
            header0: header_flags,
            header1: material_count,
            meshes,
        })
    }

    #[must_use]
    pub fn total_vertices(&self) -> usize {
        self.meshes.iter().map(Mesh::vertex_count).sum()
    }

    #[must_use]
    pub fn total_indices(&self) -> usize {
        self.meshes.iter().map(|m| m.indices.len()).sum()
    }
}

fn skip_mesh_trailers(cur: &mut Cursor<'_>) -> Result<(), ModelError> {
    if cur.read_u8("shape flag")? != 0 {
        for _ in 0..cur.read_i32("shape count")?.max(0) {
            let bytes = cur.read_i32("shape bytes")?;
            if bytes < 0 || cur.remaining() < bytes as usize {
                return Err(ModelError::InvalidVertexBlock {
                    index: 0,
                    vertex_bytes: bytes,
                });
            }
            cur.take(bytes as usize);
        }
    }
    if cur.read_u8("bone range flag")? != 0 {
        let bytes = cur.read_i32("bone range bytes")?;
        if bytes < 0 || cur.remaining() < bytes as usize {
            return Err(ModelError::InvalidIndexBlock {
                index: 0,
                index_bytes: bytes,
            });
        }
        cur.take(bytes as usize);
    }
    Ok(())
}

fn decode_vertex(chunk: &[u8], layout: VertexLayout) -> Vertex {
    let f = |off: usize| f32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    let vec3 = |at: Option<usize>| at.map_or([0.0; 3], |off| [f(off), f(off + 4), f(off + 8)]);
    Vertex {
        position: vec3(layout.position),
        normal: vec3(layout.normal),
        tangent: layout.tangent.map_or([0.0, 0.0, 0.0, 1.0], |at| {
            [f(at), f(at + 4), f(at + 8), f(at + 12)]
        }),
        uv: layout.uv.map_or([0.0; 2], |at| [f(at), f(at + 4)]),
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let start = self.offset;
        self.offset += n;
        &self.data[start..start + n]
    }

    fn need(&self, what: &'static str, n: usize) -> Result<(), ModelError> {
        if self.remaining() < n {
            return Err(ModelError::UnexpectedEof {
                what,
                offset: self.offset,
                needed: n,
                available: self.remaining(),
            });
        }
        Ok(())
    }

    fn read_u8(&mut self, what: &'static str) -> Result<u8, ModelError> {
        self.need(what, 1)?;
        Ok(self.take(1)[0])
    }

    fn read_i32(&mut self, what: &'static str) -> Result<i32, ModelError> {
        self.need(what, 4)?;
        let b = self.take(4);
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_f32x6(&mut self, what: &'static str) -> Result<[f32; 6], ModelError> {
        self.need(what, 24)?;
        let b = self.take(24);
        let mut out = [0.0f32; 6];
        for (i, slot) in out.iter_mut().enumerate() {
            let o = i * 4;
            *slot = f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        }
        Ok(out)
    }

    fn read_cstring(&mut self, what: &'static str) -> Result<String, ModelError> {
        self.need(what, 1)?;
        let start = self.offset;
        while self.offset < self.data.len() && self.data[self.offset] != 0 {
            self.offset += 1;
        }
        let bytes = &self.data[start..self.offset];
        let s = String::from_utf8_lossy(bytes).into_owned();
        if self.offset < self.data.len() {
            self.offset += 1;
        }
        Ok(s)
    }
}

pub const PUPPET_VERTEX_STRIDE: usize = 80;
const PUPPET_MARKER_SIZE: usize = 9;
pub const PUPPET_POSITION_OFFSET: usize = 0;
pub const PUPPET_NORMAL_OFFSET: usize = 12;
pub const PUPPET_TANGENT_OFFSET: usize = 24;
pub const PUPPET_BONE_INDEX_OFFSET: usize = 40;
pub const PUPPET_BONE_WEIGHT_OFFSET: usize = 56;
pub const PUPPET_UV_OFFSET: usize = 72;

pub const PUPPET_LEGACY_VERTEX_STRIDE: usize = 52;

const ATTR_POSITION: u32 = 0x1;
const ATTR_NORMAL: u32 = 0x2;
const ATTR_TANGENT: u32 = 0x4;
const ATTR_BLEND_INDICES: u32 = 0x0080_0000;
const ATTR_BLEND_WEIGHTS: u32 = 0x0100_0000;
const ATTR_SKINNED: u32 = ATTR_BLEND_INDICES | ATTR_BLEND_WEIGHTS;

const VERTEX_ATTRIBUTES: [(u32, usize); 26] = [
    (ATTR_POSITION, 12),
    (0x0001_0000, 16),
    (0x0200_0000, 12),
    (ATTR_NORMAL, 12),
    (ATTR_TANGENT, 16),
    (ATTR_BLEND_INDICES, 16),
    (ATTR_BLEND_WEIGHTS, 16),
    (0x8, 8),
    (0x10, 12),
    (0x20, 16),
    (0x40, 8),
    (0x80, 12),
    (0x100, 16),
    (0x200, 8),
    (0x400, 12),
    (0x800, 16),
    (0x1000, 8),
    (0x2000, 12),
    (0x4000, 16),
    (0x0002_0000, 8),
    (0x0004_0000, 12),
    (0x0008_0000, 16),
    (0x0010_0000, 8),
    (0x0020_0000, 12),
    (0x0040_0000, 16),
    (0x8000, 16),
];

const TEXCOORD_ATTRIBUTES: u32 = 0x8
    | 0x10
    | 0x20
    | 0x40
    | 0x80
    | 0x100
    | 0x200
    | 0x400
    | 0x800
    | 0x1000
    | 0x2000
    | 0x4000
    | 0x8000
    | 0x0002_0000
    | 0x0004_0000
    | 0x0008_0000
    | 0x0010_0000
    | 0x0020_0000
    | 0x0040_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct VertexLayout {
    stride: usize,
    position: Option<usize>,
    normal: Option<usize>,
    tangent: Option<usize>,
    bone_indices: Option<usize>,
    bone_weights: Option<usize>,
    uv: Option<usize>,
}

impl VertexLayout {
    fn for_flags(flags: u32) -> Self {
        let mut layout = VertexLayout::default();
        let mut offset = 0;
        for (mask, size) in VERTEX_ATTRIBUTES {
            if flags & mask == 0 {
                continue;
            }
            let slot = match mask {
                ATTR_POSITION => &mut layout.position,
                ATTR_NORMAL => &mut layout.normal,
                ATTR_TANGENT => &mut layout.tangent,
                ATTR_BLEND_INDICES => &mut layout.bone_indices,
                ATTR_BLEND_WEIGHTS => &mut layout.bone_weights,
                _ if mask & TEXCOORD_ATTRIBUTES != 0 && layout.uv.is_none() => &mut layout.uv,
                _ => {
                    offset += size;
                    continue;
                }
            };
            *slot = Some(offset);
            offset += size;
        }
        layout.stride = offset;
        layout
    }
}

fn puppet_version_number(version: &str) -> Option<u32> {
    version.strip_prefix("MDLV")?.parse().ok()
}

#[derive(Debug, Error)]
pub enum PuppetError {
    #[error(
        "unsupported puppet model header {header:?} (expected \"MDLV0013\", \"MDLV0016\", \"MDLV0019\", \"MDLV0021\" or \"MDLV0023\")"
    )]
    BadMagic { header: String },

    #[error("no usable puppet mesh block found before the MDLS marker")]
    NoMeshBlock,

    #[error("invalid puppet mesh index {index} (>= vertex count {vertex_count})")]
    InvalidIndex { index: u16, vertex_count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PuppetVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 4],
    pub bone_indices: [u32; 4],
    pub bone_weights: [f32; 4],
    pub uv: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PuppetBone {
    pub name: String,
    pub parent: i32,
    pub transform: [f32; 16],
}

impl PuppetBone {
    #[must_use]
    pub const fn translation(&self) -> [f32; 3] {
        [self.transform[12], self.transform[13], self.transform[14]]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PuppetAttachment {
    pub name: String,
    pub bone: usize,
    pub transform: [f32; 16],
}

impl PuppetAttachment {
    #[must_use]
    pub const fn translation(&self) -> [f32; 3] {
        [self.transform[12], self.transform[13], self.transform[14]]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PuppetKey {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PuppetTrack {
    pub bone: usize,
    pub keys: Vec<PuppetKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PuppetAnimation {
    pub id: u32,
    pub name: String,
    pub mode: String,
    pub fps: f32,
    pub frames: u32,
    pub tracks: Vec<PuppetTrack>,
    #[serde(default)]
    pub shapes: Vec<Vec<f32>>,
}

impl PuppetAnimation {
    #[must_use]
    pub fn key_at(&self, track: &PuppetTrack, time: f32) -> Option<PuppetKey> {
        let keys = track.keys.len();
        if keys == 0 {
            return None;
        }
        if keys == 1 || self.fps <= 0.0 {
            return track.keys.first().copied();
        }
        let last = (keys - 1) as f32;
        let position = (time * self.fps).rem_euclid(last);
        let lower = position.floor();
        let blend = position - lower;
        let first = track.keys.get(lower as usize)?;
        let second = track.keys.get(lower as usize + 1).unwrap_or(first);
        let mix = |a: [f32; 3], b: [f32; 3]| {
            [
                a[0] + (b[0] - a[0]) * blend,
                a[1] + (b[1] - a[1]) * blend,
                a[2] + (b[2] - a[2]) * blend,
            ]
        };
        Some(PuppetKey {
            translation: mix(first.translation, second.translation),
            rotation: mix(first.rotation, second.rotation),
            scale: mix(first.scale, second.scale),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PuppetMesh {
    pub version: String,
    pub vertices: Vec<PuppetVertex>,
    pub indices: Vec<u16>,
    pub bones: Vec<PuppetBone>,
    pub attachments: Vec<PuppetAttachment>,
    pub animations: Vec<PuppetAnimation>,
}

impl PuppetMesh {
    #[must_use]
    pub fn attachment(&self, name: &str) -> Option<&PuppetAttachment> {
        self.attachments.iter().find(|point| point.name == name)
    }

    #[must_use]
    pub fn anchor(&self, name: &str, animation: Option<&PuppetAnimation>, time: f32) -> Option<[f32; 3]> {
        let point = self.attachment(name)?;
        let local = point.translation();
        let world = self.bone_world(animation, time);
        let Some(matrix) = world.get(point.bone) else {
            return Some(local);
        };
        Some(puppet_skin_point(local, *matrix))
    }

    fn bone_world(&self, animation: Option<&PuppetAnimation>, time: f32) -> Vec<[f32; 16]> {
        let mut world: Vec<[f32; 16]> = Vec::with_capacity(self.bones.len());
        for (index, bone) in self.bones.iter().enumerate() {
            let local = animation
                .and_then(|a| {
                    a.tracks
                        .iter()
                        .find(|track| track.bone == index)
                        .and_then(|track| a.key_at(track, time))
                })
                .map_or(bone.transform, key_matrix);
            let up = usize::try_from(bone.parent)
                .ok()
                .filter(|parent| *parent < index)
                .and_then(|parent| world.get(parent).copied());
            world.push(up.map_or(local, |parent| matrix_mul(local, parent)));
        }
        world
    }

    #[must_use]
    pub fn animation(&self, id: u32) -> Option<&PuppetAnimation> {
        let mut playable = self.animations.iter().filter(|a| !a.tracks.is_empty());
        let first = playable.next()?;
        if first.id == id || playable.next().is_none() {
            return Some(first);
        }
        self.animations
            .iter()
            .find(|animation| animation.id == id && !animation.tracks.is_empty())
    }

    #[must_use]
    pub fn pose(&self, animation: Option<&PuppetAnimation>, time: f32) -> Vec<[f32; 16]> {
        let rest = self.bone_world(None, 0.0);
        let now = self.bone_world(animation, time);
        rest.iter()
            .zip(now.iter())
            .map(|(bind, posed)| matrix_invert(*bind).map_or(IDENTITY, |back| matrix_mul(back, *posed)))
            .collect()
    }
}

pub(crate) const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

pub(crate) fn key_matrix(key: PuppetKey) -> [f32; 16] {
    let (sx, cx) = key.rotation[0].sin_cos();
    let (sy, cy) = key.rotation[1].sin_cos();
    let (sz, cz) = key.rotation[2].sin_cos();
    let rotation = [
        cy * cz,
        cy * sz,
        -sy,
        0.0,
        sx * sy * cz - cx * sz,
        sx * sy * sz + cx * cz,
        sx * cy,
        0.0,
        cx * sy * cz + sx * sz,
        cx * sy * sz - sx * cz,
        cx * cy,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    let mut out = rotation;
    for row in 0..3 {
        for column in 0..3 {
            if let (Some(slot), Some(scale)) = (out.get_mut(row * 4 + column), key.scale.get(row)) {
                *slot *= scale;
            }
        }
    }
    out[12] = key.translation[0];
    out[13] = key.translation[1];
    out[14] = key.translation[2];
    out
}

pub(crate) fn matrix_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0_f32; 16];
    for row in 0..4 {
        for column in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a.get(row * 4 + k).copied().unwrap_or(0.0)
                    * b.get(k * 4 + column).copied().unwrap_or(0.0);
            }
            if let Some(slot) = out.get_mut(row * 4 + column) {
                *slot = sum;
            }
        }
    }
    out
}

pub(crate) fn matrix_invert(m: [f32; 16]) -> Option<[f32; 16]> {
    let at = |row: usize, column: usize| m.get(row * 4 + column).copied().unwrap_or(0.0);
    let determinant = at(0, 0) * (at(1, 1) * at(2, 2) - at(1, 2) * at(2, 1))
        - at(0, 1) * (at(1, 0) * at(2, 2) - at(1, 2) * at(2, 0))
        + at(0, 2) * (at(1, 0) * at(2, 1) - at(1, 1) * at(2, 0));
    if determinant.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / determinant;
    let mut out = IDENTITY;
    let cofactor = [
        (at(1, 1) * at(2, 2) - at(1, 2) * at(2, 1)) * inv,
        (at(0, 2) * at(2, 1) - at(0, 1) * at(2, 2)) * inv,
        (at(0, 1) * at(1, 2) - at(0, 2) * at(1, 1)) * inv,
        (at(1, 2) * at(2, 0) - at(1, 0) * at(2, 2)) * inv,
        (at(0, 0) * at(2, 2) - at(0, 2) * at(2, 0)) * inv,
        (at(0, 2) * at(1, 0) - at(0, 0) * at(1, 2)) * inv,
        (at(1, 0) * at(2, 1) - at(1, 1) * at(2, 0)) * inv,
        (at(0, 1) * at(2, 0) - at(0, 0) * at(2, 1)) * inv,
        (at(0, 0) * at(1, 1) - at(0, 1) * at(1, 0)) * inv,
    ];
    for row in 0..3 {
        for column in 0..3 {
            if let (Some(slot), Some(value)) = (out.get_mut(row * 4 + column), cofactor.get(row * 3 + column))
            {
                *slot = *value;
            }
        }
    }
    for column in 0..3 {
        let mut sum = 0.0;
        for k in 0..3 {
            sum += at(3, k) * out.get(k * 4 + column).copied().unwrap_or(0.0);
        }
        if let Some(slot) = out.get_mut(12 + column) {
            *slot = -sum;
        }
    }
    Some(out)
}

#[must_use]
pub fn puppet_skin_point(point: [f32; 3], matrix: [f32; 16]) -> [f32; 3] {
    let at = |index: usize| matrix.get(index).copied().unwrap_or(0.0);
    [
        point[0] * at(0) + point[1] * at(4) + point[2] * at(8) + at(12),
        point[0] * at(1) + point[1] * at(5) + point[2] * at(9) + at(13),
        point[0] * at(2) + point[1] * at(6) + point[2] * at(10) + at(14),
    ]
}

const PUPPET_BONE_MATRIX_FLOATS: usize = 16;

fn read_f32(data: &[u8], at: usize) -> Option<f32> {
    let bytes = data.get(at..at + 4)?;
    Some(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_cstring(data: &[u8], at: usize, stop: usize) -> Option<(String, usize)> {
    let tail = data.get(at..stop)?;
    let len = tail.iter().position(|b| *b == 0)?;
    let text = std::str::from_utf8(tail.get(..len)?).ok()?;
    Some((text.to_owned(), at + len + 1))
}

fn parse_puppet_attachments(data: &[u8], mdls_offset: usize) -> Vec<PuppetAttachment> {
    let Some(block) = data
        .get(mdls_offset..)
        .and_then(|tail| tail.windows(4).position(|w| w == b"MDAT"))
        .map(|at| mdls_offset + at)
    else {
        return Vec::new();
    };
    let stop = data
        .get(block..)
        .and_then(|tail| tail.windows(4).position(|w| w == b"MDLA"))
        .map_or(data.len(), |at| block + at);
    let Some(count) = data
        .get(block + 13..block + 15)
        .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
    else {
        return Vec::new();
    };
    if count == 0 || count > 4096 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(count);
    let mut at = block + 15;
    for _ in 0..count {
        let Some(bone) = data
            .get(at..at + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
        else {
            break;
        };
        let Some((name, after)) = read_cstring(data, at + 2, stop) else {
            break;
        };
        let Some(transform) = read_affine(data, after, stop) else {
            break;
        };
        out.push(PuppetAttachment {
            name,
            bone,
            transform,
        });
        at = after + PUPPET_BONE_MATRIX_FLOATS * 4;
    }
    out
}

const PUPPET_BONE_MATRIX_BYTES: u32 = 64;
const PUPPET_MAX_BONES: u32 = 128;
const PUPPET_MAX_CLIPS: u32 = 4096;
const PUPPET_KEY_BYTES: usize = 36;
const PUPPET_WEIGHT_BYTES: usize = 4;
const PUPPET_BLEND_RULE_BYTES: usize = 24;
const PUPPET_CLIP_RANGE_BYTES: usize = 18;

fn read_affine(data: &[u8], at: usize, stop: usize) -> Option<[f32; 16]> {
    if at + PUPPET_BONE_MATRIX_FLOATS * 4 > stop {
        return None;
    }
    let mut transform = [0.0_f32; PUPPET_BONE_MATRIX_FLOATS];
    for (slot, value) in transform.iter_mut().enumerate() {
        *value = read_f32(data, at + slot * 4)?;
    }
    let affine = (transform[15] - 1.0).abs() < 1e-3
        && transform[3].abs() < 1e-3
        && transform[7].abs() < 1e-3
        && transform[11].abs() < 1e-3
        && transform.iter().all(|value| value.is_finite());
    affine.then_some(transform)
}

struct PuppetCursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> PuppetCursor<'a> {
    const fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(bytes)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }

    fn cstring(&mut self) -> Option<String> {
        let tail = self.data.get(self.at..)?;
        let len = tail.iter().position(|b| *b == 0)?;
        let text = String::from_utf8_lossy(&tail[..len]).into_owned();
        self.at += len + 1;
        Some(text)
    }

    fn block_header(&mut self) -> Option<(String, u32, usize)> {
        let tag = self.cstring()?;
        let version = tag.get(4..)?.parse::<u32>().ok()?;
        let end = self.u32()? as usize;
        Some((tag, version, end))
    }

    fn matrix(&mut self) -> Option<[f32; 16]> {
        let at = self.at;
        let bytes = self.take(PUPPET_BONE_MATRIX_FLOATS * 4)?;
        read_affine(self.data, at, at + bytes.len())
    }

    fn sized(&mut self, expect: usize) -> Option<&'a [u8]> {
        let bytes = self.u32()? as usize;
        if bytes != expect {
            return None;
        }
        self.take(bytes)
    }

    fn weights(&mut self, keys: usize) -> Option<Vec<f32>> {
        let bytes = self.sized(keys * PUPPET_WEIGHT_BYTES)?;
        Some(
            bytes
                .as_chunks::<PUPPET_WEIGHT_BYTES>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
        )
    }

    fn keys(&mut self, keys: usize) -> Option<Vec<PuppetKey>> {
        let bytes = self.sized(keys * PUPPET_KEY_BYTES)?;
        Some(
            bytes
                .as_chunks::<PUPPET_KEY_BYTES>()
                .0
                .iter()
                .map(|key| {
                    let value = |slot: usize| {
                        f32::from_le_bytes([
                            key[slot * 4],
                            key[slot * 4 + 1],
                            key[slot * 4 + 2],
                            key[slot * 4 + 3],
                        ])
                    };
                    PuppetKey {
                        translation: [value(0), value(1), value(2)],
                        rotation: [value(3), value(4), value(5)],
                        scale: [value(6), value(7), value(8)],
                    }
                })
                .collect(),
        )
    }
}

struct PuppetSkeleton {
    bones: Vec<PuppetBone>,
    end: usize,
    extras: usize,
    constraints: usize,
}

fn parse_puppet_skeleton(data: &[u8], mdls_offset: usize) -> Option<PuppetSkeleton> {
    let mut cursor = PuppetCursor::new(data, mdls_offset);
    let (tag, version, end) = cursor.block_header()?;
    if !tag.starts_with("MDLS") {
        return None;
    }
    let count = cursor.u32()?;
    if count > PUPPET_MAX_BONES {
        return None;
    }
    let mut bones = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = cursor.cstring()?;
        cursor.u32()?;
        let parent = cursor.u32()? as i32;
        if cursor.u32()? != PUPPET_BONE_MATRIX_BYTES {
            return None;
        }
        let transform = cursor.matrix()?;
        cursor.cstring()?;
        bones.push(PuppetBone {
            name,
            parent,
            transform,
        });
    }
    let mut extras = 0;
    let mut constraints = 0;
    if version >= 2 {
        extras = usize::from(cursor.u16()?);
        for _ in 0..extras {
            cursor.cstring()?;
            cursor.u32()?;
            cursor.u32()?;
            cursor.take(PUPPET_BONE_MATRIX_FLOATS * 4)?;
        }
        if cursor.u8()? != 0 {
            cursor.take((bones.len() + extras) * PUPPET_BONE_MATRIX_FLOATS * 4)?;
        }
        constraints = cursor.u32()? as usize;
        for _ in 0..constraints {
            cursor.u32()?;
            cursor.f32()?;
            cursor.f32()?;
            let flags = if version >= 4 { cursor.u32()? } else { 0 };
            if flags & 2 != 0 {
                cursor.f32()?;
                cursor.f32()?;
            }
        }
    }
    Some(PuppetSkeleton {
        bones,
        end,
        extras,
        constraints,
    })
}

fn find_puppet_block(data: &[u8], mut at: usize, tag: &str) -> Option<usize> {
    while at < data.len() {
        let mut cursor = PuppetCursor::new(data, at);
        let (found, _, end) = cursor.block_header()?;
        if found.starts_with(tag) {
            return Some(at);
        }
        if end <= at || end > data.len() {
            return None;
        }
        at = end;
    }
    None
}

fn parse_puppet_animations(data: &[u8], skeleton: &PuppetSkeleton, meshes: u32) -> Vec<PuppetAnimation> {
    let Some(block) = find_puppet_block(data, skeleton.end, "MDLA") else {
        return Vec::new();
    };
    let mut cursor = PuppetCursor::new(data, block);
    let Some((_, version, _)) = cursor.block_header() else {
        return Vec::new();
    };
    let Some(count) = cursor.u32().filter(|count| *count <= PUPPET_MAX_CLIPS) else {
        return Vec::new();
    };
    let mut animations = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let Some(animation) = parse_one_animation(&mut cursor, version, skeleton, meshes) else {
            break;
        };
        animations.push(animation);
    }
    animations
}

fn parse_one_animation(
    cursor: &mut PuppetCursor<'_>,
    version: u32,
    skeleton: &PuppetSkeleton,
    meshes: u32,
) -> Option<PuppetAnimation> {
    let id = u32::try_from(cursor.u64()?).ok()?;
    let name = cursor.cstring()?;
    let mode = cursor.cstring()?;
    let fps = cursor.f32()?;
    let frames = cursor.u32()?;
    let flags = cursor.u32()?;
    let track_count = cursor.u32()?;
    if track_count > PUPPET_MAX_BONES {
        return None;
    }
    let keys = frames.checked_add(1)? as usize;
    let mut tracks = Vec::with_capacity(track_count as usize);
    for bone in 0..track_count as usize {
        cursor.u32()?;
        let keys = cursor.keys(keys)?;
        tracks.push(PuppetTrack { bone, keys });
    }
    let mut shapes = Vec::new();
    if version > 1 {
        for _ in 0..skeleton.extras {
            cursor.u32()?;
            cursor.keys(keys)?;
        }
        for _ in 0..skeleton.constraints {
            cursor.u32()?;
            cursor.weights(keys)?;
        }
    }
    if version >= 3 {
        let shape_count = cursor.u32()?;
        if shape_count > PUPPET_MAX_CLIPS {
            return None;
        }
        for _ in 0..shape_count {
            cursor.u32()?;
            shapes.push(cursor.weights(keys)?);
        }
        if cursor.u8()? != 0 {
            for _ in 0..track_count {
                cursor.u32()?;
                cursor.weights(keys)?;
            }
        }
    }
    if version >= 4 && cursor.u8()? != 0 {
        for _ in 0..meshes {
            let rule = cursor.u32()?;
            if rule & 1 != 0 {
                cursor.u32()?;
                for _ in 0..cursor.u16()? {
                    cursor.u16()?;
                    cursor.weights(keys)?;
                }
            }
        }
    }
    if version >= 5 {
        cursor.take(PUPPET_BLEND_RULE_BYTES)?;
    }
    if version >= 6 && cursor.u8()? != 0 {
        for _ in 0..track_count {
            cursor.u32()?;
            cursor.weights(keys)?;
        }
    }
    if flags & 1 != 0 {
        cursor.take(PUPPET_CLIP_RANGE_BYTES)?;
    }
    for _ in 0..cursor.u32()? {
        cursor.u32()?;
        cursor.cstring()?;
    }
    Some(PuppetAnimation {
        id,
        name,
        mode,
        fps,
        frames,
        tracks,
        shapes,
    })
}

fn puppet_mesh_count(data: &[u8]) -> u32 {
    read_u32(data, PUPPET_MARKER_SIZE + 8).unwrap_or(1)
}

impl PuppetMesh {
    pub fn parse(data: &[u8]) -> Result<Self, PuppetError> {
        let version = if data.len() >= PUPPET_MARKER_SIZE {
            String::from_utf8_lossy(&data[..8]).into_owned()
        } else {
            String::new()
        };
        let Some(number) = puppet_version_number(&version).filter(|number| *number <= 23) else {
            return Err(PuppetError::BadMagic { header: version });
        };

        let mdls_offset = find_mdls(data);
        let blocks = parse_puppet_mesh_blocks(data, number);
        let block = blocks
            .iter()
            .find(|block| block.flags & ATTR_SKINNED == ATTR_SKINNED)
            .or_else(|| blocks.first())
            .ok_or(PuppetError::NoMeshBlock)?;
        let layout = VertexLayout::for_flags(block.flags);
        if layout.stride == 0 || !block.vertex_bytes.is_multiple_of(layout.stride) {
            return Err(PuppetError::NoMeshBlock);
        }

        let vertex_count = block.vertex_bytes / layout.stride;
        let index_count = block.index_bytes / 2;

        let mut vertices = Vec::with_capacity(vertex_count);
        for i in 0..vertex_count {
            let base = block.vertices_offset + i * layout.stride;
            let rec = &data[base..base + layout.stride];
            vertices.push(decode_puppet_vertex(rec, layout));
        }

        let mut indices = Vec::with_capacity(index_count);
        for i in 0..index_count {
            let o = block.indices_offset + i * 2;
            let index = u16::from_le_bytes([data[o], data[o + 1]]);
            if index as usize >= vertex_count {
                return Err(PuppetError::InvalidIndex { index, vertex_count });
            }
            indices.push(index);
        }

        let skeleton = parse_puppet_skeleton(data, mdls_offset);
        let animations = skeleton
            .as_ref()
            .map(|skeleton| parse_puppet_animations(data, skeleton, puppet_mesh_count(data)))
            .unwrap_or_default();
        Ok(Self {
            version,
            vertices,
            indices,
            bones: skeleton.map(|skeleton| skeleton.bones).unwrap_or_default(),
            attachments: parse_puppet_attachments(data, mdls_offset),
            animations,
        })
    }

    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }
}

struct PuppetMeshBlock {
    flags: u32,
    vertices_offset: usize,
    vertex_bytes: usize,
    indices_offset: usize,
    index_bytes: usize,
}

fn find_mdls(data: &[u8]) -> usize {
    let mut offset = PUPPET_MARKER_SIZE;
    while offset + 4 <= data.len() {
        if &data[offset..offset + 4] == b"MDLS" {
            return offset;
        }
        offset += 1;
    }
    data.len()
}

fn parse_puppet_mesh_blocks(data: &[u8], version: u32) -> Vec<PuppetMeshBlock> {
    let mut blocks = Vec::new();
    read_puppet_mesh_blocks(data, version, &mut blocks);
    blocks
}

fn read_puppet_mesh_blocks(data: &[u8], version: u32, blocks: &mut Vec<PuppetMeshBlock>) -> Option<()> {
    let mut cur = PuppetCursor::new(data, PUPPET_MARKER_SIZE);
    let header_flags = cur.u32()?;
    let materials = cur.u32()?;
    let meshes = cur.u32()?;
    if materials > PUPPET_MAX_BONES || meshes > PUPPET_MAX_BONES {
        return None;
    }
    for _ in 0..meshes {
        for _ in 0..materials {
            cur.cstring()?;
        }
        let mut flags = header_flags;
        if version >= 4 && cur.u32()? & 2 != 0 {
            cur.u32()?;
        }
        if version >= 17 {
            cur.take(24)?;
        }
        if version > 14 {
            flags = cur.u32()?;
        }
        let vertex_bytes = cur.u32()? as usize;
        let vertices_offset = cur.at;
        cur.take(vertex_bytes)?;
        let index_bytes = cur.u32()? as usize;
        let indices_offset = cur.at;
        cur.take(index_bytes)?;
        if version >= 21 {
            if cur.u8()? != 0 {
                for _ in 0..cur.u32()? {
                    let bytes = cur.u32()? as usize;
                    cur.take(bytes)?;
                }
            }
            if cur.u8()? != 0 {
                let bytes = cur.u32()? as usize;
                cur.take(bytes)?;
            }
        }
        blocks.push(PuppetMeshBlock {
            flags,
            vertices_offset,
            vertex_bytes,
            indices_offset,
            index_bytes,
        });
    }
    Some(())
}

fn decode_puppet_vertex(chunk: &[u8], layout: VertexLayout) -> PuppetVertex {
    let f = |off: usize| f32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    let u = |off: usize| u32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    let vec3 = |at: Option<usize>| at.map_or([0.0; 3], |off| [f(off), f(off + 4), f(off + 8)]);
    PuppetVertex {
        position: vec3(layout.position),
        normal: vec3(layout.normal),
        tangent: layout.tangent.map_or([0.0, 0.0, 0.0, 1.0], |at| {
            [f(at), f(at + 4), f(at + 8), f(at + 12)]
        }),
        bone_indices: layout
            .bone_indices
            .map_or([0; 4], |at| [u(at), u(at + 4), u(at + 8), u(at + 12)]),
        bone_weights: layout.bone_weights.map_or([1.0, 0.0, 0.0, 0.0], |at| {
            [f(at), f(at + 4), f(at + 8), f(at + 12)]
        }),
        uv: layout.uv.map_or([0.0; 2], |at| [f(at), f(at + 4)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_two_mesh_model(shapes: &[usize], ranges: usize, material: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"MDLV0023");
        b.push(0);
        b.extend_from_slice(&15i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&2i32.to_le_bytes());
        for mesh in 0..2 {
            b.extend_from_slice(material.as_bytes());
            b.push(0);
            b.extend_from_slice(&0i32.to_le_bytes());
            for value in [-1.0f32, -2.0, -3.0, 4.0, 5.0, 6.0] {
                b.extend_from_slice(&value.to_le_bytes());
            }
            b.extend_from_slice(&15i32.to_le_bytes());
            let verts = mesh + 1;
            b.extend_from_slice(&((verts * VERTEX_STRIDE) as i32).to_le_bytes());
            b.resize(b.len() + verts * VERTEX_STRIDE, 0);
            b.extend_from_slice(&6i32.to_le_bytes());
            b.extend_from_slice(&[0u8; 6]);
            if shapes.is_empty() {
                b.push(0);
            } else {
                b.push(1);
                b.extend_from_slice(&(shapes.len() as i32).to_le_bytes());
                for bytes in shapes {
                    b.extend_from_slice(&(*bytes as i32).to_le_bytes());
                    b.resize(b.len() + bytes, 0);
                }
            }
            if ranges == 0 {
                b.push(0);
            } else {
                b.push(1);
                b.extend_from_slice(&(ranges as i32).to_le_bytes());
                b.resize(b.len() + ranges, 0);
            }
        }
        b
    }

    #[test]
    fn a_mesh_shape_block_does_not_derail_the_next_mesh() {
        let bytes = synth_two_mesh_model(&[24], 16, "materials/test.json");
        let model = Model::parse(&bytes).expect("both meshes are read");
        assert_eq!(model.meshes.len(), 2);
        assert_eq!(model.meshes[1].vertex_count(), 2);
    }

    #[test]
    fn a_material_path_with_a_space_is_still_a_mesh_header() {
        let bytes = synth_two_mesh_model(&[], 0, "materials/little head.json");
        let model = Model::parse(&bytes).expect("a spaced path is a path");
        assert_eq!(model.meshes.len(), 2);
        assert_eq!(model.meshes[0].material_ref, "materials/little head.json");
    }

    #[test]
    fn a_model_with_empty_trailers_still_reads() {
        let bytes = synth_two_mesh_model(&[], 0, "materials/test.json");
        let model = Model::parse(&bytes).expect("meshes back to back");
        assert_eq!(model.meshes.len(), 2);
    }

    fn synth_model(version: &str, verts: u32, tris: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(version.as_bytes());
        b.push(0);
        b.extend_from_slice(&15i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(b"materials/test.json");
        b.push(0);
        b.extend_from_slice(&0i32.to_le_bytes());
        for v in [-1.0f32, -2.0, -3.0, 4.0, 5.0, 6.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&15i32.to_le_bytes());
        let vbytes = verts * VERTEX_STRIDE as u32;
        b.extend_from_slice(&(vbytes as i32).to_le_bytes());
        for i in 0..verts {
            b.extend_from_slice(&(i as f32).to_le_bytes());
            for _ in 1..12 {
                b.extend_from_slice(&0.0f32.to_le_bytes());
            }
        }
        let ibytes = tris * 3 * 2;
        b.extend_from_slice(&(ibytes as i32).to_le_bytes());
        for t in 0..tris {
            for k in 0..3u16 {
                b.extend_from_slice(&((t as u16 + k) % verts as u16).to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn parses_synthetic_model() {
        let bytes = synth_model("MDLV0017", 4, 2);
        let m = Model::parse(&bytes).expect("parse");
        assert_eq!(m.version, "MDLV0017");
        assert_eq!(m.header0, 15);
        assert_eq!(m.header1, 1);
        assert_eq!(m.meshes.len(), 1);
        let mesh = &m.meshes[0];
        assert_eq!(mesh.material_ref, "materials/test.json");
        assert_eq!(mesh.bbox_min, [-1.0, -2.0, -3.0]);
        assert_eq!(mesh.bbox_max, [4.0, 5.0, 6.0]);
        assert_eq!(mesh.vertex_count(), 4);
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(m.total_vertices(), 4);
        assert_eq!(m.total_indices(), 6);
        let positions: Vec<f32> = mesh.vertices().map(|v| v.position[0]).collect();
        assert_eq!(positions, vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = synth_model("MDLV0017", 1, 1);
        bytes[0..4].copy_from_slice(b"XXXX");
        match Model::parse(&bytes) {
            Err(ModelError::BadMagic { header }) => assert!(header.starts_with("XXXX")),
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn truncated_header_is_typed_error() {
        let bytes = synth_model("MDLV0017", 1, 1);
        let truncated = &bytes[..11];
        assert!(matches!(
            Model::parse(truncated),
            Err(ModelError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn truncated_vertex_block_is_typed_error() {
        let full = synth_model("MDLV0017", 8, 4);
        let truncated = &full[..full.len() - 100];
        match Model::parse(truncated) {
            Err(ModelError::InvalidVertexBlock { index, .. }) => assert_eq!(index, 0),
            Err(ModelError::UnexpectedEof { .. }) => {}
            other => panic!("expected typed truncation error, got {other:?}"),
        }
    }

    #[test]
    fn odd_index_block_is_rejected() {
        let mut bytes = synth_model("MDLV0017", 4, 2);
        let ibytes_payload = 2u32 * 3 * 2;
        let pos = bytes.len() - ibytes_payload as usize - 4;
        bytes[pos..pos + 4].copy_from_slice(&5i32.to_le_bytes());
        assert!(matches!(
            Model::parse(&bytes),
            Err(ModelError::InvalidIndexBlock { .. })
        ));
    }

    #[test]
    fn huge_mesh_count_does_not_panic() {
        let mut bytes = synth_model("MDLV0017", 1, 1);
        let pos = 8 + 1 + 4 + 4;
        bytes[pos..pos + 4].copy_from_slice(&2_000_000_000i32.to_le_bytes());
        assert!(matches!(
            Model::parse(&bytes),
            Err(ModelError::UnexpectedEof { .. }) | Err(ModelError::InvalidVertexBlock { .. })
        ));
    }

    #[test]
    fn negative_mesh_count_is_rejected() {
        let mut bytes = synth_model("MDLV0017", 1, 1);
        let pos = 8 + 1 + 4 + 4;
        bytes[pos..pos + 4].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(matches!(
            Model::parse(&bytes),
            Err(ModelError::InvalidMeshCount { count: -1 })
        ));
    }

    use crate::pkg::Pkg;
    use std::path::PathBuf;

    const CORPUS_DIR: &str = "/home/aiko/.steam/steam/steamapps/workshop/content/431960";

    fn corpus_dir() -> Option<PathBuf> {
        let dir = std::env::var_os("KIRIE_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(CORPUS_DIR));
        if dir.is_dir() {
            Some(dir)
        } else {
            eprintln!("skipping corpus test: {} not found", dir.display());
            None
        }
    }

    fn models_in_pkg(pkg: &Pkg<'_>) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in pkg.entries() {
            if let Ok(payload) = pkg.read(entry)
                && payload.len() >= 4
                && &payload[..4] == b"MDLV"
            {
                let name = entry.name_str().unwrap_or("<non-utf8>").to_owned();
                out.push((name, payload.to_vec()));
            }
        }
        out
    }

    #[test]
    fn parses_real_starscape_model() {
        let Some(dir) = corpus_dir() else { return };
        let pkg_path = dir.join("3047596375").join("scene.pkg");
        if !pkg_path.is_file() {
            eprintln!("skipping: {} not present", pkg_path.display());
            return;
        }
        let bytes = std::fs::read(&pkg_path).unwrap();
        let pkg = Pkg::parse(&bytes).expect("parse pkg");
        let models = models_in_pkg(&pkg);
        assert_eq!(models.len(), 1, "Starscape has exactly one .mdl");
        let (name, payload) = &models[0];
        assert_eq!(name, "models/space boi/space boi.mdl");

        let model = Model::parse(payload).expect("parse Starscape model");
        assert_eq!(model.version, "MDLV0017");
        assert_eq!(model.meshes.len(), 2);
        assert_eq!(model.total_vertices(), 61296);
        assert_eq!(model.total_indices(), 241992);

        for mesh in &model.meshes {
            assert!(mesh.vertex_count() > 0);
            assert!(!mesh.indices.is_empty());
            let max_index = mesh.indices.iter().copied().max().unwrap();
            assert!((max_index as usize) < mesh.vertex_count());
            for axis in 0..3 {
                assert!(mesh.bbox_min[axis] <= mesh.bbox_max[axis]);
            }
            assert!(mesh.material_ref.starts_with("materials/"));
            let first = mesh.vertices().next().unwrap();
            for axis in 0..3 {
                assert!(
                    first.position[axis] >= mesh.bbox_min[axis] - 1.0
                        && first.position[axis] <= mesh.bbox_max[axis] + 1.0,
                    "vertex axis {axis} out of bbox"
                );
            }
        }
    }

    #[test]
    fn corpus_every_model_scans_like_the_reference() {
        let Some(dir) = corpus_dir() else { return };
        let mut items_with_models = 0usize;
        let mut total_models = 0usize;
        let mut parsed = 0usize;
        let mut ref_rejected = 0usize;
        let mut versions: std::collections::BTreeMap<String, (usize, usize)> = Default::default();

        for item in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let pkg_path = item.path().join("scene.pkg");
            if !pkg_path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&pkg_path).unwrap();
            let Ok(pkg) = Pkg::parse(&bytes) else { continue };
            let models = models_in_pkg(&pkg);
            if models.is_empty() {
                continue;
            }
            items_with_models += 1;
            for (name, payload) in &models {
                total_models += 1;
                let version = String::from_utf8_lossy(&payload[..8]).into_owned();
                match Model::parse(payload) {
                    Ok(model) => {
                        assert!(!model.meshes.is_empty(), "model {name} has no meshes");
                        assert!(model.total_vertices() > 0, "model {name} has no vertices");
                        assert_eq!(model.version, version);
                        parsed += 1;
                        versions.entry(version).or_default().0 += 1;
                    }
                    Err(ModelError::InvalidVertexBlock { .. }) => {
                        ref_rejected += 1;
                        versions.entry(version).or_default().1 += 1;
                    }
                    Err(e) => panic!("model {name} failed unexpectedly: {e}"),
                }
            }
        }

        eprintln!(
            "corpus: {total_models} model(s) across {items_with_models} item(s); \
             {parsed} parsed, {ref_rejected} reference-rejected; per-version (parsed,rejected) {versions:?}"
        );
        assert!(parsed >= 1, "expected at least the MDLV0017 model to parse");
    }

    const SYNTH_FLAGS: u32 =
        ATTR_POSITION | ATTR_NORMAL | ATTR_TANGENT | 0x8 | ATTR_BLEND_INDICES | ATTR_BLEND_WEIGHTS;
    const SYNTH_LEGACY_FLAGS: u32 = ATTR_POSITION | 0x8 | ATTR_BLEND_INDICES | ATTR_BLEND_WEIGHTS;

    fn synth_mesh_header(b: &mut Vec<u8>, version: &str, flags: u32) {
        let number = puppet_version_number(version).expect("version");
        b.extend_from_slice(b"materials/synth.json\0");
        if number >= 4 {
            b.extend_from_slice(&0u32.to_le_bytes());
        }
        if number >= 17 {
            b.extend_from_slice(&[0u8; 24]);
        }
        if number > 14 {
            b.extend_from_slice(&flags.to_le_bytes());
        }
    }

    fn synth_mesh_trailer(b: &mut Vec<u8>, version: &str) {
        if puppet_version_number(version).expect("version") >= 21 {
            b.push(0);
            b.push(0);
        }
    }

    fn synth_puppet_body(version: &str, verts: u32, tris: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(version.as_bytes());
        b.push(0);
        b.extend_from_slice(&SYNTH_FLAGS.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        synth_mesh_header(&mut b, version, SYNTH_FLAGS);
        let vbytes = verts * PUPPET_VERTEX_STRIDE as u32;
        b.extend_from_slice(&vbytes.to_le_bytes());
        for i in 0..verts {
            let mut rec = [0u8; PUPPET_VERTEX_STRIDE];
            rec[PUPPET_POSITION_OFFSET..PUPPET_POSITION_OFFSET + 4]
                .copy_from_slice(&(i as f32).to_le_bytes());
            rec[PUPPET_BONE_INDEX_OFFSET..PUPPET_BONE_INDEX_OFFSET + 4].copy_from_slice(&i.to_le_bytes());
            rec[PUPPET_BONE_WEIGHT_OFFSET..PUPPET_BONE_WEIGHT_OFFSET + 4]
                .copy_from_slice(&1.0f32.to_le_bytes());
            rec[PUPPET_UV_OFFSET..PUPPET_UV_OFFSET + 4].copy_from_slice(&(i as f32 * 0.5).to_le_bytes());
            b.extend_from_slice(&rec);
        }
        let ibytes = tris * 3 * 2;
        b.extend_from_slice(&ibytes.to_le_bytes());
        for t in 0..tris {
            for k in 0..3u16 {
                b.extend_from_slice(&((t as u16 + k) % verts as u16).to_le_bytes());
            }
        }
        synth_mesh_trailer(&mut b, version);
        b
    }

    fn append_block(bytes: &mut Vec<u8>, block: &[u8]) {
        let end = (bytes.len() + block.len()) as u32;
        bytes.extend_from_slice(block);
        let at = bytes.len() - block.len() + PUPPET_MARKER_SIZE;
        bytes[at..at + 4].copy_from_slice(&end.to_le_bytes());
    }

    fn synth_puppet(version: &str, verts: u32, tris: u32) -> Vec<u8> {
        let mut b = synth_puppet_body(version, verts, tris);
        append_block(&mut b, &synth_skeleton(1, &[]));
        b
    }

    #[test]
    fn parses_synthetic_puppet() {
        let bytes = synth_puppet("MDLV0023", 6, 4);
        let m = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(m.version, "MDLV0023");
        assert_eq!(m.vertex_count(), 6);
        assert_eq!(m.indices.len(), 12);
        let v = &m.vertices[3];
        assert_eq!(v.position[0], 3.0);
        assert_eq!(v.bone_indices[0], 3);
        assert_eq!(v.bone_weights[0], 1.0);
        assert_eq!(v.uv[0], 1.5);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertex_count()));
    }

    fn synth_legacy_puppet(verts: u32, tris: u32) -> Vec<u8> {
        let layout = VertexLayout::for_flags(SYNTH_LEGACY_FLAGS);
        assert_eq!(layout.stride, PUPPET_LEGACY_VERTEX_STRIDE);
        let mut b = Vec::new();
        b.extend_from_slice(b"MDLV0013");
        b.push(0);
        b.extend_from_slice(&SYNTH_LEGACY_FLAGS.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        synth_mesh_header(&mut b, "MDLV0013", SYNTH_LEGACY_FLAGS);
        let vbytes = verts * PUPPET_LEGACY_VERTEX_STRIDE as u32;
        b.extend_from_slice(&vbytes.to_le_bytes());
        for i in 0..verts {
            let mut rec = [0u8; PUPPET_LEGACY_VERTEX_STRIDE];
            let at = layout.position.expect("position");
            rec[at..at + 4].copy_from_slice(&(i as f32).to_le_bytes());
            let at = layout.bone_indices.expect("bone indices");
            rec[at..at + 4].copy_from_slice(&i.to_le_bytes());
            let at = layout.bone_weights.expect("bone weights");
            rec[at..at + 4].copy_from_slice(&1.0f32.to_le_bytes());
            let at = layout.uv.expect("uv");
            rec[at..at + 4].copy_from_slice(&(i as f32 * 0.25).to_le_bytes());
            b.extend_from_slice(&rec);
        }
        let ibytes = tris * 3 * 2;
        b.extend_from_slice(&ibytes.to_le_bytes());
        for t in 0..tris {
            for k in 0..3u16 {
                b.extend_from_slice(&((t as u16 + k) % verts as u16).to_le_bytes());
            }
        }
        b.extend_from_slice(b"MDLS");
        b.extend_from_slice(&[0u8; 8]);
        b
    }

    #[test]
    fn puppet_bones_are_named_and_carry_a_transform() {
        let mut bytes = synth_puppet("MDLV0023", 3, 1);
        append_block(&mut bytes, &synth_attachments(&[(2, "head", [-32.5, 116.4])]));
        bytes.extend_from_slice(b"MDLA0006\0");

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(
            mesh.attachments.len(),
            1,
            "the header before the first bone is skipped"
        );
        let point = mesh.attachment("head").expect("an attachment named head");
        assert_eq!(point.bone, 2);
        assert_eq!(point.translation(), [-32.5, 116.4, 0.0]);
        assert_eq!(mesh.anchor("head", None, 0.0), Some([-32.5, 116.4, 0.0]));
        assert!(mesh.attachment("missing").is_none());
    }

    fn translation_matrix(translation: [f32; 2]) -> [f32; 16] {
        [
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            translation[0],
            translation[1],
            0.0,
            1.0,
        ]
    }

    fn synth_skeleton(version: u32, bones: &[[f32; 2]]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(format!("MDLS{version:04}\0").as_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(bones.len() as u32).to_le_bytes());
        for (index, translation) in bones.iter().enumerate() {
            b.push(0);
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&(if index == 0 { -1i32 } else { 0 }).to_le_bytes());
            b.extend_from_slice(&64u32.to_le_bytes());
            for value in translation_matrix(*translation) {
                b.extend_from_slice(&value.to_le_bytes());
            }
            b.push(0);
        }
        if version >= 2 {
            b.extend_from_slice(&0u16.to_le_bytes());
            b.push(0);
            b.extend_from_slice(&0u32.to_le_bytes());
        }
        b
    }

    struct SynthClip {
        id: u64,
        name: &'static str,
        mode: &'static str,
        frames: u32,
        tracks: Vec<Vec<[f32; 2]>>,
        shapes: Vec<Vec<f32>>,
        ranged: bool,
        events: Vec<(u32, &'static str)>,
    }

    fn synth_clip(id: u32, keys: &[[f32; 2]]) -> SynthClip {
        SynthClip {
            id: u64::from(id),
            name: "Animation 1",
            mode: "loop",
            frames: 0,
            tracks: keys.iter().map(|key| vec![*key]).collect(),
            shapes: Vec::new(),
            ranged: false,
            events: Vec::new(),
        }
    }

    fn synth_animation(version: u32, meshes: u32, clips: &[SynthClip]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(format!("MDLA{version:04}\0").as_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(clips.len() as u32).to_le_bytes());
        for clip in clips {
            let keys = clip.frames as usize + 1;
            b.extend_from_slice(&clip.id.to_le_bytes());
            b.extend_from_slice(clip.name.as_bytes());
            b.push(0);
            b.extend_from_slice(clip.mode.as_bytes());
            b.push(0);
            b.extend_from_slice(&12.0f32.to_le_bytes());
            b.extend_from_slice(&clip.frames.to_le_bytes());
            b.extend_from_slice(&u32::from(clip.ranged).to_le_bytes());
            b.extend_from_slice(&(clip.tracks.len() as u32).to_le_bytes());
            for track in &clip.tracks {
                assert_eq!(track.len(), keys);
                b.extend_from_slice(&0u32.to_le_bytes());
                b.extend_from_slice(&((keys * PUPPET_KEY_BYTES) as u32).to_le_bytes());
                for key in track {
                    for value in [key[0], key[1], 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
                        b.extend_from_slice(&value.to_le_bytes());
                    }
                }
            }
            let weights = |b: &mut Vec<u8>, values: &[f32]| {
                assert_eq!(values.len(), keys);
                b.extend_from_slice(&((keys * PUPPET_WEIGHT_BYTES) as u32).to_le_bytes());
                for value in values {
                    b.extend_from_slice(&value.to_le_bytes());
                }
            };
            if version >= 3 {
                b.extend_from_slice(&(clip.shapes.len() as u32).to_le_bytes());
                for shape in &clip.shapes {
                    b.extend_from_slice(&7u32.to_le_bytes());
                    weights(&mut b, shape);
                }
                b.push(1);
                for _ in &clip.tracks {
                    b.extend_from_slice(&0u32.to_le_bytes());
                    weights(&mut b, &vec![0.5; keys]);
                }
            }
            if version >= 4 {
                b.push(1);
                for mesh in 0..meshes {
                    let rule = (mesh + 1) & 1;
                    b.extend_from_slice(&rule.to_le_bytes());
                    if rule == 1 {
                        b.extend_from_slice(&0u32.to_le_bytes());
                        b.extend_from_slice(&1u16.to_le_bytes());
                        b.extend_from_slice(&0u16.to_le_bytes());
                        weights(&mut b, &vec![1.0; keys]);
                    }
                }
            }
            if version >= 5 {
                b.extend_from_slice(&[0; PUPPET_BLEND_RULE_BYTES]);
            }
            if version >= 6 {
                b.push(1);
                for _ in &clip.tracks {
                    b.extend_from_slice(&0u32.to_le_bytes());
                    weights(&mut b, &vec![0.25; keys]);
                }
            }
            if clip.ranged {
                b.extend_from_slice(&[0; PUPPET_CLIP_RANGE_BYTES]);
            }
            b.extend_from_slice(&(clip.events.len() as u32).to_le_bytes());
            for (frame, name) in &clip.events {
                b.extend_from_slice(&frame.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                b.push(0);
            }
        }
        b
    }

    #[test]
    fn puppet_reads_an_unnamed_skeleton_and_its_animation() {
        let mut bytes = synth_puppet_body("MDLV0023", 3, 1);
        append_block(
            &mut bytes,
            &synth_skeleton(1, &[[-178.3, 532.2], [-519.0, 339.0]]),
        );
        append_block(
            &mut bytes,
            &synth_animation(1, 1, &[synth_clip(167, &[[-178.3, 532.2], [-118.0, 139.3]])]),
        );

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(mesh.bones.len(), 2);
        assert_eq!(mesh.bones[0].parent, -1);
        assert_eq!(mesh.bones[1].parent, 0);
        let animation = mesh.animation(167).expect("animation 167");
        assert_eq!(animation.tracks.len(), 2);
        assert_eq!(animation.tracks[1].bone, 1);

        let pose = mesh.pose(Some(animation), 0.0);
        assert!((pose[0][12]).abs() < 0.01, "an unmoved bone poses to identity");
        assert!(
            (pose[1][12] - 401.0).abs() < 1.0,
            "a moved bone carries the animation delta, got {}",
            pose[1][12]
        );
    }

    #[test]
    fn puppet_reads_every_clip_behind_the_version_trailers() {
        for version in 1..=6u32 {
            let mut bytes = synth_puppet_body("MDLV0023", 3, 1);
            append_block(
                &mut bytes,
                &synth_skeleton(version.min(4), &[[0.0, 0.0], [1.0, 2.0]]),
            );
            let clips = [
                SynthClip {
                    id: 448,
                    name: "idle",
                    mode: "loop",
                    frames: 2,
                    tracks: vec![vec![[0.0, 0.0]; 3], vec![[1.0, 2.0], [2.0, 2.0], [1.0, 2.0]]],
                    shapes: vec![vec![0.0, 0.5, 1.0]],
                    ranged: true,
                    events: vec![(1, "step")],
                },
                SynthClip {
                    id: 330,
                    name: "wave",
                    mode: "single",
                    frames: 1,
                    tracks: vec![vec![[0.0, 0.0]; 2], vec![[1.0, 2.0]; 2]],
                    shapes: vec![vec![1.0, 1.0], vec![0.0, 0.0]],
                    ranged: false,
                    events: Vec::new(),
                },
                synth_clip(301, &[[0.0, 0.0], [1.0, 2.0]]),
            ];
            append_block(&mut bytes, &synth_animation(version, 1, &clips));

            let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
            assert_eq!(mesh.bones.len(), 2, "MDLA{version}");
            let ids: Vec<u32> = mesh.animations.iter().map(|clip| clip.id).collect();
            assert_eq!(ids, [448, 330, 301], "MDLA{version} keeps every clip");
            let idle = mesh.animation(448).expect("idle");
            assert_eq!(idle.name, "idle");
            assert_eq!(idle.frames, 2);
            assert_eq!(idle.tracks[1].keys.len(), 3);
            assert_eq!(idle.tracks[1].keys[1].translation, [2.0, 2.0, 0.0]);
            let wave = mesh.animation(330).expect("wave");
            assert_eq!(wave.mode, "single");
            if version >= 3 {
                assert_eq!(idle.shapes, vec![vec![0.0, 0.5, 1.0]], "MDLA{version}");
                assert_eq!(wave.shapes.len(), 2, "MDLA{version}");
            } else {
                assert!(idle.shapes.is_empty());
            }
        }
    }

    #[test]
    fn puppet_keeps_the_clips_parsed_before_a_truncated_one() {
        let mut bytes = synth_puppet_body("MDLV0023", 3, 1);
        append_block(&mut bytes, &synth_skeleton(3, &[[0.0, 0.0]]));
        let clips = [synth_clip(1, &[[0.0, 0.0]]), synth_clip(2, &[[0.0, 0.0]])];
        let animation = synth_animation(6, 1, &clips);
        let cut = animation.len() - 20;
        append_block(&mut bytes, &animation[..cut]);

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(mesh.animations.len(), 1);
        assert_eq!(mesh.animations[0].id, 1);
    }

    #[test]
    fn puppet_rejects_a_track_whose_bytes_disagree_with_its_frames() {
        let mut bytes = synth_puppet_body("MDLV0023", 3, 1);
        append_block(&mut bytes, &synth_skeleton(1, &[[0.0, 0.0]]));
        let mut animation = synth_animation(1, 1, &[synth_clip(5, &[[0.0, 0.0]])]);
        let frames_at = PUPPET_MARKER_SIZE + 4 + 4 + 8 + "Animation 1\0".len() + "loop\0".len() + 4;
        animation[frames_at..frames_at + 4].copy_from_slice(&3u32.to_le_bytes());
        append_block(&mut bytes, &animation);

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert!(mesh.animations.is_empty());
    }

    fn synth_attachments(points: &[(u16, &str, [f32; 2])]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"MDAT0001\0");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(points.len() as u16).to_le_bytes());
        for (bone, name, translation) in points {
            b.extend_from_slice(&bone.to_le_bytes());
            b.extend_from_slice(name.as_bytes());
            b.push(0);
            for value in translation_matrix(*translation) {
                b.extend_from_slice(&value.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn an_attachment_anchors_in_its_bone_chain() {
        let mut bytes = synth_puppet_body("MDLV0023", 3, 1);
        append_block(&mut bytes, &synth_skeleton(1, &[[10.0, 20.0], [3.0, 4.0]]));
        append_block(&mut bytes, &synth_attachments(&[(1, "hook", [5.0, 6.0])]));
        append_block(
            &mut bytes,
            &synth_animation(1, 1, &[synth_clip(9, &[[10.0, 20.0], [3.0, 4.0]])]),
        );

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(mesh.bones.len(), 2);
        assert_eq!(mesh.animations.len(), 1, "MDLA is reached through the MDAT block");
        assert_eq!(mesh.attachment("hook").map(|p| p.bone), Some(1));
        let at = mesh.anchor("hook", None, 0.0).expect("an anchor");
        assert!(
            (at[0] - 18.0).abs() < 0.01 && (at[1] - 30.0).abs() < 0.01,
            "the anchor composes bone 0, bone 1 and the local offset, got {at:?}"
        );
    }

    #[test]
    fn vertex_flags_lay_out_the_strides_the_corpus_uses() {
        let skinned = VertexLayout::for_flags(0x0180_000F);
        assert_eq!(skinned.stride, PUPPET_VERTEX_STRIDE);
        assert_eq!(skinned.position, Some(PUPPET_POSITION_OFFSET));
        assert_eq!(skinned.normal, Some(PUPPET_NORMAL_OFFSET));
        assert_eq!(skinned.tangent, Some(PUPPET_TANGENT_OFFSET));
        assert_eq!(skinned.bone_indices, Some(PUPPET_BONE_INDEX_OFFSET));
        assert_eq!(skinned.bone_weights, Some(PUPPET_BONE_WEIGHT_OFFSET));
        assert_eq!(skinned.uv, Some(PUPPET_UV_OFFSET));

        let legacy = VertexLayout::for_flags(0x0180_0009);
        assert_eq!(legacy.stride, PUPPET_LEGACY_VERTEX_STRIDE);
        assert_eq!(legacy.position, Some(0));
        assert_eq!(legacy.bone_indices, Some(12));
        assert_eq!(legacy.bone_weights, Some(28));
        assert_eq!(legacy.uv, Some(44));
        assert_eq!(legacy.normal, None);

        let channels = VertexLayout::for_flags(0x0080_0021);
        assert_eq!(channels.stride, 44);
        assert_eq!(channels.bone_indices, Some(12));
        assert_eq!(channels.uv, Some(28));
    }

    #[test]
    fn legacy_puppet_reads_four_wide_bone_indices() {
        let mut bytes = synth_legacy_puppet(6, 4);
        let block = &parse_puppet_mesh_blocks(&bytes, 13)[0];
        let layout = VertexLayout::for_flags(block.flags);
        assert_eq!(layout.bone_indices, Some(12));
        let slot = block.vertices_offset + 3 * PUPPET_LEGACY_VERTEX_STRIDE + 12;
        bytes[slot..slot + 4].copy_from_slice(&5u32.to_le_bytes());
        bytes[slot + 4..slot + 8].copy_from_slice(&9u32.to_le_bytes());

        let mesh = PuppetMesh::parse(&bytes).expect("parse legacy puppet");
        assert_eq!(
            [mesh.vertices[3].bone_indices[0], mesh.vertices[3].bone_indices[1]],
            [5, 9],
            "blend indices are four u32s, not four bytes"
        );
    }

    #[test]
    fn puppet_accepts_mdlv0013_with_its_own_stride() {
        let bytes = synth_legacy_puppet(6, 4);
        let m = PuppetMesh::parse(&bytes).expect("parse legacy puppet");
        assert_eq!(m.version, "MDLV0013");
        assert_eq!(m.vertex_count(), 6);
        assert_eq!(m.indices.len(), 12);
        let v = &m.vertices[3];
        assert_eq!(v.position[0], 3.0);
        assert_eq!(v.bone_indices[0], 3);
        assert_eq!(v.bone_weights[0], 1.0);
        assert_eq!(v.uv[0], 0.75);
        assert_eq!(v.tangent, [0.0, 0.0, 0.0, 1.0]);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertex_count()));
    }

    #[test]
    fn puppet_accepts_mdlv0021() {
        let bytes = synth_puppet("MDLV0021", 3, 1);
        assert!(PuppetMesh::parse(&bytes).is_ok());
    }

    #[test]
    fn puppet_rejects_bad_magic() {
        let mut bytes = synth_puppet("MDLV0023", 3, 1);
        bytes[5] = b'X';
        match PuppetMesh::parse(&bytes) {
            Err(PuppetError::BadMagic { .. }) => {}
            other => panic!("expected BadMagic, got {other:?}"),
        }
        let future = b"MDLV0031\0".to_vec();
        assert!(matches!(
            PuppetMesh::parse(&future),
            Err(PuppetError::BadMagic { .. })
        ));
        let short = b"MDLV0017\0".to_vec();
        assert!(matches!(PuppetMesh::parse(&short), Err(PuppetError::NoMeshBlock)));
    }

    #[test]
    fn puppet_rejects_out_of_range_index() {
        let mut bytes = synth_puppet("MDLV0023", 3, 1);
        let idx0 = parse_puppet_mesh_blocks(&bytes, 23)[0].indices_offset;
        bytes[idx0..idx0 + 2].copy_from_slice(&99u16.to_le_bytes());
        match PuppetMesh::parse(&bytes) {
            Err(PuppetError::InvalidIndex {
                index: 99,
                vertex_count: 3,
            }) => {}
            other => panic!("expected InvalidIndex, got {other:?}"),
        }
    }

    #[test]
    fn puppet_no_block_before_mdls_is_typed_error() {
        let mut bytes = b"MDLV0023\0".to_vec();
        bytes.extend_from_slice(b"MDLS");
        bytes.extend_from_slice(&[0u8; 16]);
        assert!(matches!(PuppetMesh::parse(&bytes), Err(PuppetError::NoMeshBlock)));
    }

    #[test]
    fn puppet_never_panics_on_random_input() {
        let mut bytes = b"MDLV0023\0".to_vec();
        bytes.extend(
            std::iter::successors(Some(1u8), |n| Some(n.wrapping_mul(31).wrapping_add(7))).take(4096),
        );
        let _ = PuppetMesh::parse(&bytes);
    }

    #[test]
    fn corpus_puppets_yield_every_clip_the_header_promises() {
        let roots = std::env::var("KIRIE_PUPPET_CORPUS")
            .map(|value| value.split(':').map(PathBuf::from).collect::<Vec<_>>())
            .unwrap_or_else(|_| corpus_dir().into_iter().collect());
        let mut checked = 0;
        for root in roots.iter().filter(|root| root.is_dir()) {
            for item in std::fs::read_dir(root).unwrap().flatten() {
                let pkg_path = item.path().join("scene.pkg");
                let Ok(bytes) = std::fs::read(&pkg_path) else {
                    continue;
                };
                let Ok(pkg) = Pkg::parse(&bytes) else { continue };
                for entry in pkg.entries() {
                    let name = entry.name_str().unwrap_or("").to_owned();
                    let Ok(payload) = pkg.read(entry) else { continue };
                    if !name.ends_with(".mdl") || !payload.starts_with(b"MDLV") {
                        continue;
                    }
                    let Some(mdla) = payload.windows(4).position(|w| w == b"MDLA") else {
                        continue;
                    };
                    let promised = read_u32(payload, mdla + 13).unwrap();
                    let mesh = PuppetMesh::parse(payload).expect("parse puppet");
                    assert_eq!(
                        mesh.animations.len() as u32,
                        promised,
                        "{}/{name} clips",
                        item.file_name().to_string_lossy()
                    );
                    assert!(
                        !mesh.bones.is_empty(),
                        "{}/{name} bones",
                        item.file_name().to_string_lossy()
                    );
                    for clip in &mesh.animations {
                        for track in &clip.tracks {
                            assert_eq!(
                                track.keys.len() as u32,
                                clip.frames + 1,
                                "{name} {} keys",
                                clip.id
                            );
                        }
                    }
                    checked += 1;
                }
            }
        }
        eprintln!("checked {checked} puppet models");
    }

    #[test]
    fn corpus_parses_scene_3428443753_puppets() {
        let Some(dir) = corpus_dir() else { return };
        let pkg_path = dir.join("3428443753").join("scene.pkg");
        if !pkg_path.is_file() {
            eprintln!("skipping: {} not present", pkg_path.display());
            return;
        }
        let bytes = std::fs::read(&pkg_path).unwrap();
        let pkg = Pkg::parse(&bytes).expect("parse pkg");
        let mut puppets = Vec::new();
        for entry in pkg.entries() {
            let name = entry.name_str().unwrap_or("");
            if name.ends_with("_puppet.mdl")
                && let Ok(payload) = pkg.read(entry)
            {
                puppets.push((name.to_owned(), PuppetMesh::parse(payload).expect("parse puppet")));
            }
        }
        assert_eq!(puppets.len(), 3, "scene 3428443753 has three puppet meshes");
        for (name, mesh) in &puppets {
            assert_eq!(mesh.version, "MDLV0023", "{name}");
            assert!(mesh.vertex_count() > 0, "{name} has vertices");
            assert!(!mesh.indices.is_empty(), "{name} has indices");
            assert!(mesh.indices.len().is_multiple_of(3), "{name} whole triangles");
            let max = mesh.indices.iter().copied().max().unwrap();
            assert!((max as usize) < mesh.vertex_count(), "{name} indices in range");
            let w: f32 = mesh.vertices[0].bone_weights.iter().sum();
            assert!(
                (w - 1.0).abs() < 0.01,
                "{name} first-vertex weights sum ~1 (got {w})"
            );
        }
    }
}
