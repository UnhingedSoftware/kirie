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

#[derive(Debug, Error)]
pub enum PuppetError {
    #[error("unsupported puppet model header {header:?} (expected \"MDLV0021\" or \"MDLV0023\")")]
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
pub struct PuppetMesh {
    pub version: String,
    pub vertices: Vec<PuppetVertex>,
    pub indices: Vec<u16>,
}

impl PuppetMesh {
    pub fn parse(data: &[u8]) -> Result<Self, PuppetError> {
        let version = if data.len() >= PUPPET_MARKER_SIZE {
            String::from_utf8_lossy(&data[..8]).into_owned()
        } else {
            String::new()
        };
        if version != "MDLV0021" && version != "MDLV0023" {
            return Err(PuppetError::BadMagic { header: version });
        }

        let mdls_offset = find_mdls(data);
        let block = find_puppet_mesh_block(data, mdls_offset).ok_or(PuppetError::NoMeshBlock)?;

        let vertex_count = block.vertex_bytes / PUPPET_VERTEX_STRIDE;
        let vertices_offset = block.header_offset + PUPPET_MESH_HEADER_SIZE;
        let indices_offset = vertices_offset + block.vertex_bytes + 4;
        let index_count = block.index_bytes / 2;

        let mut vertices = Vec::with_capacity(vertex_count);
        for i in 0..vertex_count {
            let base = vertices_offset + i * PUPPET_VERTEX_STRIDE;
            let rec = &data[base..base + PUPPET_VERTEX_STRIDE];
            vertices.push(decode_puppet_vertex(rec));
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

fn find_puppet_mesh_block(data: &[u8], mdls_offset: usize) -> Option<PuppetMeshBlock> {
    let read_u32 =
        |off: usize| -> u32 { u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) };
    let mut offset = PUPPET_MARKER_SIZE;
    while offset + PUPPET_MESH_HEADER_SIZE + 4 < mdls_offset {
        let vertex_bytes = read_u32(offset + 4) as usize;
        let vertices_offset = offset + PUPPET_MESH_HEADER_SIZE;
        let index_length_offset = vertices_offset + vertex_bytes;

        if vertex_bytes == 0
            || !vertex_bytes.is_multiple_of(PUPPET_VERTEX_STRIDE)
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

fn decode_puppet_vertex(chunk: &[u8]) -> PuppetVertex {
    let f = |off: usize| f32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    let u = |off: usize| u32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
    PuppetVertex {
        position: [
            f(PUPPET_POSITION_OFFSET),
            f(PUPPET_POSITION_OFFSET + 4),
            f(PUPPET_POSITION_OFFSET + 8),
        ],
        normal: [
            f(PUPPET_NORMAL_OFFSET),
            f(PUPPET_NORMAL_OFFSET + 4),
            f(PUPPET_NORMAL_OFFSET + 8),
        ],
        tangent: [
            f(PUPPET_TANGENT_OFFSET),
            f(PUPPET_TANGENT_OFFSET + 4),
            f(PUPPET_TANGENT_OFFSET + 8),
            f(PUPPET_TANGENT_OFFSET + 12),
        ],
        bone_indices: [
            u(PUPPET_BONE_INDEX_OFFSET),
            u(PUPPET_BONE_INDEX_OFFSET + 4),
            u(PUPPET_BONE_INDEX_OFFSET + 8),
            u(PUPPET_BONE_INDEX_OFFSET + 12),
        ],
        bone_weights: [
            f(PUPPET_BONE_WEIGHT_OFFSET),
            f(PUPPET_BONE_WEIGHT_OFFSET + 4),
            f(PUPPET_BONE_WEIGHT_OFFSET + 8),
            f(PUPPET_BONE_WEIGHT_OFFSET + 12),
        ],
        uv: [f(PUPPET_UV_OFFSET), f(PUPPET_UV_OFFSET + 4)],
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
