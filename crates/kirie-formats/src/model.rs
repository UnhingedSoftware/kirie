use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const VERTEX_STRIDE: usize = 48;

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
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertex_data.len() / VERTEX_STRIDE
    }

    pub fn vertices(&self) -> impl Iterator<Item = Vertex> + '_ {
        self.vertex_data
            .as_chunks::<VERTEX_STRIDE>()
            .0
            .iter()
            .map(decode_vertex)
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

        let header0 = cur.read_i32("header0")?;
        let header1 = cur.read_i32("header1")?;
        let mesh_count = cur.read_i32("meshCount")?;
        if mesh_count < 0 {
            return Err(ModelError::InvalidMeshCount { count: mesh_count });
        }

        let mut meshes = Vec::new();
        for index in 0..mesh_count as usize {
            let material_ref = cur.read_cstring("materialRef")?;
            let _reserved = cur.read_i32("mesh reserved word")?;
            let bbox = cur.read_f32x6("bbox")?;
            let flags = cur.read_i32("flags")?;

            let vertex_bytes = cur.read_i32("vertexBytes")?;
            if vertex_bytes <= 0
                || !(vertex_bytes as usize).is_multiple_of(VERTEX_STRIDE)
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
            header0,
            header1,
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

fn decode_vertex(chunk: &[u8; VERTEX_STRIDE]) -> Vertex {
    let f = |off: usize| f32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    Vertex {
        position: [f(POSITION_OFFSET), f(POSITION_OFFSET + 4), f(POSITION_OFFSET + 8)],
        normal: [f(NORMAL_OFFSET), f(NORMAL_OFFSET + 4), f(NORMAL_OFFSET + 8)],
        tangent: [
            f(TANGENT_OFFSET),
            f(TANGENT_OFFSET + 4),
            f(TANGENT_OFFSET + 8),
            f(TANGENT_OFFSET + 12),
        ],
        uv: [f(UV_OFFSET), f(UV_OFFSET + 4)],
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
const PUPPET_MESH_HEADER_SIZE: usize = 8;
pub const PUPPET_POSITION_OFFSET: usize = 0;
pub const PUPPET_NORMAL_OFFSET: usize = 12;
pub const PUPPET_TANGENT_OFFSET: usize = 24;
pub const PUPPET_BONE_INDEX_OFFSET: usize = 40;
pub const PUPPET_BONE_WEIGHT_OFFSET: usize = 56;
pub const PUPPET_UV_OFFSET: usize = 72;

pub const PUPPET_LEGACY_VERTEX_STRIDE: usize = 52;
const PUPPET_LEGACY_BONE_INDEX_OFFSET: usize = 12;
const PUPPET_LEGACY_NORMAL_OFFSET: usize = 16;
const PUPPET_LEGACY_BONE_WEIGHT_OFFSET: usize = 28;
const PUPPET_LEGACY_UV_OFFSET: usize = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PuppetLayout {
    stride: usize,
    position: usize,
    normal: usize,
    tangent: Option<usize>,
    bone_indices: usize,
    wide_bone_indices: bool,
    bone_weights: usize,
    uv: usize,
}

impl PuppetLayout {
    const fn for_version(version: &str) -> Option<Self> {
        match version.as_bytes() {
            b"MDLV0021" | b"MDLV0023" => Some(Self {
                stride: PUPPET_VERTEX_STRIDE,
                position: PUPPET_POSITION_OFFSET,
                normal: PUPPET_NORMAL_OFFSET,
                tangent: Some(PUPPET_TANGENT_OFFSET),
                bone_indices: PUPPET_BONE_INDEX_OFFSET,
                wide_bone_indices: true,
                bone_weights: PUPPET_BONE_WEIGHT_OFFSET,
                uv: PUPPET_UV_OFFSET,
            }),
            b"MDLV0013" => Some(Self {
                stride: PUPPET_LEGACY_VERTEX_STRIDE,
                position: PUPPET_POSITION_OFFSET,
                normal: PUPPET_LEGACY_NORMAL_OFFSET,
                tangent: None,
                bone_indices: PUPPET_LEGACY_BONE_INDEX_OFFSET,
                wide_bone_indices: false,
                bone_weights: PUPPET_LEGACY_BONE_WEIGHT_OFFSET,
                uv: PUPPET_LEGACY_UV_OFFSET,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum PuppetError {
    #[error("unsupported puppet model header {header:?} (expected \"MDLV0013\", \"MDLV0021\" or \"MDLV0023\")")]
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
    pub attachments: Vec<PuppetBone>,
    pub animations: Vec<PuppetAnimation>,
}

impl PuppetMesh {
    #[must_use]
    pub fn bone(&self, name: &str) -> Option<&PuppetBone> {
        self.attachments
            .iter()
            .chain(self.bones.iter())
            .find(|bone| bone.name == name)
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
        self.bones
            .iter()
            .enumerate()
            .map(|(index, bone)| {
                let rest = bone.transform;
                let now = animation
                    .and_then(|a| {
                        a.tracks
                            .iter()
                            .find(|track| track.bone == index)
                            .and_then(|track| a.key_at(track, time))
                    })
                    .map_or(rest, key_matrix);
                matrix_invert(rest).map_or(IDENTITY, |back| matrix_mul(back, now))
            })
            .collect()
    }
}

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn key_matrix(key: PuppetKey) -> [f32; 16] {
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

fn matrix_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
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

fn matrix_invert(m: [f32; 16]) -> Option<[f32; 16]> {
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
            if let (Some(slot), Some(value)) = (
                out.get_mut(row * 4 + column),
                cofactor.get(row * 3 + column),
            ) {
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

fn puppet_bone_at(data: &[u8], at: usize, stop: usize) -> Option<(PuppetBone, usize)> {
    let tail = data.get(at..stop)?;
    let len = tail.iter().position(|b| *b == 0)?;
    if len == 0 || len > 63 {
        return None;
    }
    let name = std::str::from_utf8(tail.get(..len)?).ok()?;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " _-.:".contains(c))
    {
        return None;
    }

    let matrix_at = at + len + 1;
    if matrix_at + PUPPET_BONE_MATRIX_FLOATS * 4 > stop {
        return None;
    }
    let mut transform = [0.0_f32; PUPPET_BONE_MATRIX_FLOATS];
    for (slot, value) in transform.iter_mut().enumerate() {
        *value = read_f32(data, matrix_at + slot * 4)?;
    }

    let affine = (transform[15] - 1.0).abs() < 1e-3
        && transform[3].abs() < 1e-3
        && transform[7].abs() < 1e-3
        && transform[11].abs() < 1e-3
        && transform.iter().all(|value| value.is_finite());
    if !affine {
        return None;
    }

    Some((
        PuppetBone {
            name: name.to_owned(),
            parent: -1,
            transform,
        },
        matrix_at + PUPPET_BONE_MATRIX_FLOATS * 4,
    ))
}

fn parse_puppet_attachments(data: &[u8], mdls_offset: usize) -> Vec<PuppetBone> {
    let Some(block) = data
        .get(mdls_offset..)
        .and_then(|tail| tail.windows(7).position(|w| w == b"DAT0001"))
        .map(|at| mdls_offset + at + 7)
    else {
        return Vec::new();
    };
    let stop = data
        .get(block..)
        .and_then(|tail| tail.windows(4).position(|w| w == b"MDLA"))
        .map_or(data.len(), |at| block + at);

    let mut bones = Vec::new();
    let mut at = block;
    while at < stop {
        if let Some((bone, next)) = puppet_bone_at(data, at, stop) {
            bones.push(bone);
            at = next;
        } else {
            at += 1;
        }
    }
    bones
}

const PUPPET_BONE_MATRIX_BYTES: u32 = 64;

fn skeleton_bone_at(data: &[u8], at: usize, stop: usize) -> Option<(PuppetBone, usize)> {
    let tail = data.get(at..stop)?;
    let len = tail.iter().position(|b| *b == 0)?;
    if len > 63 {
        return None;
    }
    let name = std::str::from_utf8(tail.get(..len)?).ok()?;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " _-.:".contains(c))
    {
        return None;
    }

    let fields = at + len + 1;
    let read_u32 = |off: usize| -> Option<u32> {
        let b = data.get(off..off + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let parent = read_u32(fields + 4)? as i32;
    if read_u32(fields + 8)? != PUPPET_BONE_MATRIX_BYTES {
        return None;
    }

    let matrix_at = fields + 12;
    let transform = read_affine(data, matrix_at, stop)?;
    Some((
        PuppetBone {
            name: name.to_owned(),
            parent,
            transform,
        },
        matrix_at + PUPPET_BONE_MATRIX_FLOATS * 4,
    ))
}

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

const PUPPET_KEY_BYTES: usize = 36;

fn parse_puppet_animations(data: &[u8]) -> Vec<PuppetAnimation> {
    let Some(block) = data.windows(4).position(|w| w == b"MDLA") else {
        return Vec::new();
    };
    let stop = data.len();
    let Some(count) = read_u32(data, block + 13).map(|value| value as usize) else {
        return Vec::new();
    };
    if count == 0 || count > 256 {
        return Vec::new();
    }

    let mut animations = Vec::with_capacity(count);
    let mut at = block + 17;
    for _ in 0..count {
        let Some(animation) = parse_one_animation(data, &mut at, stop) else {
            break;
        };
        animations.push(animation);
    }
    animations
}

fn parse_one_animation(data: &[u8], at: &mut usize, stop: usize) -> Option<PuppetAnimation> {
    let id = read_u32(data, *at)?;
    let (name, after_name) = read_cstring(data, *at + 8, stop)?;
    let (mode, after_mode) = read_cstring(data, after_name, stop)?;
    let fps = read_f32(data, after_mode)?;
    let frames = read_u32(data, after_mode + 4)?;
    let bone_count = read_u32(data, after_mode + 12)? as usize;
    if bone_count > 4096 {
        return None;
    }

    let mut cursor = after_mode + 16;
    let mut tracks = Vec::with_capacity(bone_count);
    for bone in 0..bone_count {
        let bytes = read_u32(data, cursor + 4)? as usize;
        if bytes == 0 || !bytes.is_multiple_of(PUPPET_KEY_BYTES) || cursor + 8 + bytes > stop {
            return None;
        }
        let keys_at = cursor + 8;
        let mut keys = Vec::with_capacity(bytes / PUPPET_KEY_BYTES);
        for index in 0..bytes / PUPPET_KEY_BYTES {
            let key = keys_at + index * PUPPET_KEY_BYTES;
            let mut values = [0.0_f32; 9];
            for (slot, value) in values.iter_mut().enumerate() {
                *value = read_f32(data, key + slot * 4)?;
            }
            keys.push(PuppetKey {
                translation: [values[0], values[1], values[2]],
                rotation: [values[3], values[4], values[5]],
                scale: [values[6], values[7], values[8]],
            });
        }
        tracks.push(PuppetTrack { bone, keys });
        cursor = keys_at + bytes;
    }

    *at = cursor;
    Some(PuppetAnimation {
        id,
        name,
        mode,
        fps,
        frames,
        tracks,
    })
}

fn parse_puppet_skeleton(data: &[u8], mdls_offset: usize) -> Vec<PuppetBone> {
    let Some(count_at) = mdls_offset.checked_add(13) else {
        return Vec::new();
    };
    let Some(count) = read_u32(data, count_at).map(|value| value as usize) else {
        return Vec::new();
    };
    if count == 0 || count > 4096 {
        return Vec::new();
    }
    let stop = data
        .get(mdls_offset..)
        .and_then(|tail| tail.windows(4).position(|w| w == b"MDAT" || w == b"MDLA"))
        .map_or(data.len(), |at| mdls_offset + at);

    let mut bones = Vec::with_capacity(count);
    let mut at = count_at + 4;
    while bones.len() < count && at < stop {
        if let Some((bone, next)) = skeleton_bone_at(data, at, stop) {
            bones.push(bone);
            at = next;
        } else {
            at += 1;
        }
    }
    bones
}

impl PuppetMesh {
    pub fn parse(data: &[u8]) -> Result<Self, PuppetError> {
        let version = if data.len() >= PUPPET_MARKER_SIZE {
            String::from_utf8_lossy(&data[..8]).into_owned()
        } else {
            String::new()
        };
        let Some(layout) = PuppetLayout::for_version(&version) else {
            return Err(PuppetError::BadMagic { header: version });
        };

        let mdls_offset = find_mdls(data);
        let block =
            find_puppet_mesh_block(data, mdls_offset, layout.stride).ok_or(PuppetError::NoMeshBlock)?;

        let vertex_count = block.vertex_bytes / layout.stride;
        let vertices_offset = block.header_offset + PUPPET_MESH_HEADER_SIZE;
        let indices_offset = vertices_offset + block.vertex_bytes + 4;
        let index_count = block.index_bytes / 2;

        let mut vertices = Vec::with_capacity(vertex_count);
        for i in 0..vertex_count {
            let base = vertices_offset + i * layout.stride;
            let rec = &data[base..base + layout.stride];
            vertices.push(decode_puppet_vertex(rec, layout));
        }

        let mut indices = Vec::with_capacity(index_count);
        for i in 0..index_count {
            let o = indices_offset + i * 2;
            let index = u16::from_le_bytes([data[o], data[o + 1]]);
            if index as usize >= vertex_count {
                return Err(PuppetError::InvalidIndex { index, vertex_count });
            }
            indices.push(index);
        }

        Ok(Self {
            version,
            vertices,
            indices,
            bones: parse_puppet_skeleton(data, mdls_offset),
            attachments: parse_puppet_attachments(data, mdls_offset),
            animations: parse_puppet_animations(data),
        })
    }

    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }
}

struct PuppetMeshBlock {
    header_offset: usize,
    vertex_bytes: usize,
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

fn find_puppet_mesh_block(data: &[u8], mdls_offset: usize, stride: usize) -> Option<PuppetMeshBlock> {
    let read_u32 =
        |off: usize| -> u32 { u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) };
    let mut offset = PUPPET_MARKER_SIZE;
    while offset + PUPPET_MESH_HEADER_SIZE + 4 < mdls_offset {
        let vertex_bytes = read_u32(offset + 4) as usize;
        let vertices_offset = offset + PUPPET_MESH_HEADER_SIZE;
        let index_length_offset = vertices_offset + vertex_bytes;

        if vertex_bytes == 0
            || !vertex_bytes.is_multiple_of(stride)
            || index_length_offset + 4 > mdls_offset
        {
            offset += 1;
            continue;
        }

        let index_bytes = read_u32(index_length_offset) as usize;
        let indices_offset = index_length_offset + 4;
        if index_bytes == 0
            || !index_bytes.is_multiple_of(2 * 3)
            || indices_offset + index_bytes > mdls_offset
        {
            offset += 1;
            continue;
        }

        return Some(PuppetMeshBlock {
            header_offset: offset,
            vertex_bytes,
            index_bytes,
        });
    }
    None
}

fn decode_puppet_vertex(chunk: &[u8], layout: PuppetLayout) -> PuppetVertex {
    let f = |off: usize| f32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    let u = |off: usize| u32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    let bone = |slot: usize| -> u32 {
        if layout.wide_bone_indices {
            u(layout.bone_indices + slot * 4)
        } else {
            u32::from(chunk[layout.bone_indices + slot])
        }
    };
    PuppetVertex {
        position: [
            f(layout.position),
            f(layout.position + 4),
            f(layout.position + 8),
        ],
        normal: [f(layout.normal), f(layout.normal + 4), f(layout.normal + 8)],
        tangent: layout.tangent.map_or([0.0, 0.0, 0.0, 1.0], |at| {
            [f(at), f(at + 4), f(at + 8), f(at + 12)]
        }),
        bone_indices: [bone(0), bone(1), bone(2), bone(3)],
        bone_weights: [
            f(layout.bone_weights),
            f(layout.bone_weights + 4),
            f(layout.bone_weights + 8),
            f(layout.bone_weights + 12),
        ],
        uv: [f(layout.uv), f(layout.uv + 4)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn synth_puppet(version: &str, verts: u32, tris: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(version.as_bytes());
        b.push(0);
        b.extend_from_slice(&[0xAB; 12]);
        b.extend_from_slice(&0u32.to_le_bytes());
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
        b.extend_from_slice(b"MDLS");
        b.extend_from_slice(&[0u8; 8]);
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
        let mut b = Vec::new();
        b.extend_from_slice(b"MDLV0013");
        b.push(0);
        b.extend_from_slice(&0u32.to_le_bytes());
        let vbytes = verts * PUPPET_LEGACY_VERTEX_STRIDE as u32;
        b.extend_from_slice(&vbytes.to_le_bytes());
        for i in 0..verts {
            let mut rec = [0u8; PUPPET_LEGACY_VERTEX_STRIDE];
            rec[PUPPET_POSITION_OFFSET..PUPPET_POSITION_OFFSET + 4]
                .copy_from_slice(&(i as f32).to_le_bytes());
            rec[PUPPET_LEGACY_BONE_INDEX_OFFSET] = i as u8;
            rec[PUPPET_LEGACY_BONE_WEIGHT_OFFSET..PUPPET_LEGACY_BONE_WEIGHT_OFFSET + 4]
                .copy_from_slice(&1.0f32.to_le_bytes());
            rec[PUPPET_LEGACY_UV_OFFSET..PUPPET_LEGACY_UV_OFFSET + 4]
                .copy_from_slice(&(i as f32 * 0.25).to_le_bytes());
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
        bytes.extend_from_slice(b"DAT0001\0");
        bytes.extend_from_slice(&[0x1a, 0x98, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00]);
        bytes.extend_from_slice(b"head\0");
        let matrix: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -32.5, 116.4, 0.0, 1.0,
        ];
        for value in matrix {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(b"MDLA0006\0");

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(
            mesh.attachments.len(),
            1,
            "the header before the first bone is skipped"
        );
        let bone = mesh.bone("head").expect("a bone named head");
        assert_eq!(bone.translation(), [-32.5, 116.4, 0.0]);
        assert!(mesh.bone("missing").is_none());
    }

    fn synth_skeleton(bones: &[[f32; 2]]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"MDLS0001\0");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(bones.len() as u32).to_le_bytes());
        for (index, translation) in bones.iter().enumerate() {
            b.push(0);
            b.extend_from_slice(&(index as u32).to_le_bytes());
            b.extend_from_slice(&(if index == 0 { -1i32 } else { 0 }).to_le_bytes());
            b.extend_from_slice(&64u32.to_le_bytes());
            let matrix: [f32; 16] = [
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
            ];
            for value in matrix {
                b.extend_from_slice(&value.to_le_bytes());
            }
        }
        b
    }

    fn synth_animation(id: u32, keys: &[[f32; 2]]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"MDLA0001\0");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(b"Animation 1\0");
        b.extend_from_slice(b"loop\0");
        b.extend_from_slice(&12.0f32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for key in keys {
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&(PUPPET_KEY_BYTES as u32).to_le_bytes());
            for value in [key[0], key[1], 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
                b.extend_from_slice(&value.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn puppet_reads_an_unnamed_skeleton_and_its_animation() {
        let mut bytes = synth_puppet("MDLV0023", 3, 1);
        bytes.truncate(bytes.len() - 12);
        bytes.extend_from_slice(&synth_skeleton(&[[-178.3, 532.2], [-519.0, 339.0]]));
        bytes.extend_from_slice(&synth_animation(167, &[[-178.3, 532.2], [-118.0, 139.3]]));

        let mesh = PuppetMesh::parse(&bytes).expect("parse puppet");
        assert_eq!(mesh.bones.len(), 2);
        assert_eq!(mesh.bones[0].parent, -1);
        assert_eq!(mesh.bones[1].parent, 0);
        let animation = mesh.animation(167).expect("animation 167");
        assert_eq!(animation.tracks.len(), 2);
        assert_eq!(animation.tracks[1].bone, 1);

        let pose = mesh.pose(Some(animation), 0.0);
        assert!(
            (pose[0][12]).abs() < 0.01,
            "an unmoved bone poses to identity"
        );
        assert!(
            (pose[1][12] - 401.0).abs() < 1.0,
            "a moved bone carries the animation delta, got {}",
            pose[1][12]
        );
    }

    #[test]
    fn legacy_puppet_reads_bone_indices_from_offset_twelve() {
        let mut bytes = synth_legacy_puppet(6, 4);
        let vertices_at = 17;
        let slot = vertices_at + 3 * PUPPET_LEGACY_VERTEX_STRIDE + 12;
        bytes[slot] = 5;

        let mesh = PuppetMesh::parse(&bytes).expect("parse legacy puppet");
        assert_eq!(
            mesh.vertices[3].bone_indices[0], 5,
            "MDLV0013 stores bone indices 12 bytes into the vertex"
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
        let short = b"MDLV0017\0".to_vec();
        assert!(matches!(
            PuppetMesh::parse(&short),
            Err(PuppetError::BadMagic { .. })
        ));
    }

    #[test]
    fn puppet_rejects_out_of_range_index() {
        let mut bytes = synth_puppet("MDLV0023", 3, 1);
        let idx0 = 9 + 12 + 8 + 3 * PUPPET_VERTEX_STRIDE + 4;
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
