use naga::front::glsl::{Frontend as GlslFrontend, Options as GlslOptions};
use naga::valid::{Capabilities, ValidationFlags, Validator};

use crate::reflect::Reflection;
use crate::{Stage, TranslateError, TranslatePath, TranslatedShader};

pub fn translate(
    stage: Stage,
    filename: &str,
    modernized: String,
    reflection: Reflection,
) -> Result<TranslatedShader, TranslateError> {
    let key = shader_cache_key(stage, &modernized);
    if let Some(module) = cache_load_module(&key) {
        return Ok(TranslatedShader {
            module,
            reflection,
            path: TranslatePath::Shaderc,
            glsl: String::new(),
        });
    }

    let flat = preprocess_and_flatten(stage, filename, &modernized);
    let flat = crate::hlslmod::rewrite_modulo(&flat);
    let flat = crate::coerce::coerce_shapes(&flat);
    let flat = crate::hlslrelax::relax_hlsl_shapes(&flat);
    if let Some(dir) = std::env::var_os("KIRIE_SHADER_DUMP_ALL") {
        let stem = filename.replace(['/', '\\'], "_");
        let mut mark = 0_u64;
        for byte in flat.bytes() {
            mark = mark.wrapping_mul(0x100_0000_01b3) ^ u64::from(byte);
        }
        let at = std::path::Path::new(&dir).join(format!("{stem}.{stage:?}.{mark:016x}.glsl"));
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(&at, &flat);
        }
    }
    let flat = crate::matinverse::shadow_builtin_inverse(&flat);

    let naga_diag = match try_naga_glsl(stage, &flat).and_then(validate) {
        Ok(module) => {
            cache_store_module(&key, &module);
            return Ok(TranslatedShader {
                module,
                reflection,
                path: TranslatePath::NagaGlsl,
                glsl: flat,
            });
        }
        Err(e) => e,
    };

    let mut mended = flat.clone();
    let mut shaderc_diag = String::new();
    for _ in 0..64 {
        match try_shaderc(stage, filename, &mended).and_then(validate) {
            Ok(module) => {
                cache_store_module(&key, &module);
                return Ok(TranslatedShader {
                    module,
                    reflection,
                    path: TranslatePath::Shaderc,
                    glsl: mended,
                });
            }
            Err(e) => {
                let Some(next) = crate::repair::repair_conversion(&mended, &e) else {
                    shaderc_diag = e;
                    break;
                };
                shaderc_diag = e;
                mended = next;
            }
        }
    }

    if let Some(dir) = std::env::var_os("KIRIE_SHADER_DUMP") {
        let stem = filename.replace(['/', '\\'], "_");
        let mut mark = 0_u64;
        for byte in mended.bytes() {
            mark = mark.wrapping_mul(0x100_0000_01b3) ^ u64::from(byte);
        }
        let at = std::path::Path::new(&dir).join(format!("{stem}.{stage:?}.{mark:016x}.glsl"));
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(&at, &mended);
        }
    }

    Err(TranslateError::Compile {
        file: filename.to_string(),
        naga: naga_diag,
        shaderc: shaderc_diag,
    })
}

fn validate(module: naga::Module) -> Result<naga::Module, String> {
    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
    match validator.validate(&module) {
        Ok(_) => Ok(module),
        Err(e) => Err(format!("validation: {:?}", e.as_inner())),
    }
}

fn shaderc_options() -> Option<shaderc::CompileOptions<'static>> {
    let mut opts = shaderc::CompileOptions::new().ok()?;
    opts.set_target_env(shaderc::TargetEnv::Vulkan, shaderc::EnvVersion::Vulkan1_2 as u32);
    opts.set_target_spirv(shaderc::SpirvVersion::V1_3);
    opts.set_auto_bind_uniforms(true);
    opts.set_auto_map_locations(true);
    Some(opts)
}

fn preprocess_and_flatten(stage: Stage, filename: &str, modernized: &str) -> String {
    if modernized.contains('\0') || filename.contains('\0') {
        return modernized.to_string();
    }
    let Some(compiler) = shaderc::Compiler::new().ok() else {
        return modernized.to_string();
    };
    let Some(opts) = shaderc_options() else {
        return modernized.to_string();
    };
    let _ = stage;
    match compiler.preprocess(modernized, filename, "main", Some(&opts)) {
        Ok(pp) => flatten_array_varyings(&pp.as_text()),
        Err(_) => modernized.to_string(),
    }
}

fn try_naga_glsl(stage: Stage, src: &str) -> Result<naga::Module, String> {
    let mut frontend = GlslFrontend::default();
    let options = GlslOptions::from(stage.naga());
    frontend.parse(&options, src).map_err(|e| e.emit_to_string(src))
}

fn try_shaderc(stage: Stage, filename: &str, src: &str) -> Result<naga::Module, String> {
    if let Some(at) = src.find('\0') {
        return Err(format!("source holds a NUL byte at offset {at}"));
    }
    if filename.contains('\0') {
        return Err("file name holds a NUL byte".to_string());
    }
    let compiler = shaderc::Compiler::new().map_err(|e| e.to_string())?;
    let opts = shaderc_options().ok_or_else(|| "shaderc options unavailable".to_string())?;
    let kind = match stage {
        Stage::Vertex => shaderc::ShaderKind::Vertex,
        Stage::Fragment => shaderc::ShaderKind::Fragment,
    };
    let artifact = compiler
        .compile_into_spirv(src, kind, filename, "main", Some(&opts))
        .map_err(|e| first_error_line(&e.to_string()))?;
    let spv_opts = naga::front::spv::Options {
        adjust_coordinate_space: false,
        ..Default::default()
    };
    naga::front::spv::parse_u8_slice(artifact.as_binary_u8(), &spv_opts).map_err(|e| format!("{e:?}"))
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct UnitEntry {
    includes: Vec<(String, [u8; 32])>,
    glsl: String,
    path: crate::TranslatePath,
    reflection: crate::Reflection,
    module: naga::Module,
}

pub(crate) fn unit_cache_key(stage: Stage, source: &str, inputs: &crate::ShaderInputs) -> String {
    const UNIT_FORMAT: u32 = 1;
    let mut h = blake3::Hasher::new();
    h.update(source.as_bytes());
    h.update(&[match stage {
        Stage::Vertex => 0u8,
        Stage::Fragment => 1u8,
    }]);
    for (k, v) in &inputs.combos {
        h.update(k.as_bytes());
        h.update(&v.to_le_bytes());
        h.update(&[0xfe]);
    }
    h.update(&[0xfd]);
    for (k, v) in &inputs.override_combos {
        h.update(k.as_bytes());
        h.update(&v.to_le_bytes());
        h.update(&[0xfe]);
    }
    h.update(&[0xfd]);
    for slot in &inputs.populated_texture_slots {
        h.update(&slot.to_le_bytes());
    }
    h.update(&crate::TRANSLATOR_VERSION.to_le_bytes());
    h.update(&UNIT_FORMAT.to_le_bytes());
    h.finalize().to_hex().to_string()
}

pub(crate) fn unit_cache_load(
    key: &str,
    resolver: &dyn crate::IncludeResolver,
) -> Option<crate::TranslatedShader> {
    let bytes = std::fs::read(spirv_cache_dir()?.join(format!("{key}.tng"))).ok()?;
    let entry: UnitEntry = bincode::deserialize(&bytes).ok()?;
    for (name, hash) in &entry.includes {
        let body = resolver.resolve(name)?;
        if blake3::hash(body.as_bytes()).as_bytes() != hash {
            return None;
        }
    }
    Some(crate::TranslatedShader {
        module: entry.module,
        reflection: entry.reflection,
        path: entry.path,
        glsl: entry.glsl,
    })
}

pub(crate) fn unit_cache_store(key: &str, includes: Vec<(String, [u8; 32])>, ts: &crate::TranslatedShader) {
    let entry = UnitEntry {
        includes,
        glsl: ts.glsl.clone(),
        path: ts.path,
        reflection: ts.reflection.clone(),
        module: ts.module.clone(),
    };
    if let (Ok(bytes), Some(dir)) = (bincode::serialize(&entry), spirv_cache_dir()) {
        write_cache_atomic(&dir.join(format!("{key}.tng")), &bytes);
        maybe_prune_cache(&dir);
    }
}

fn shader_cache_key(stage: Stage, modernized: &str) -> String {
    const CACHE_FORMAT: u32 = 3;
    let mut h = blake3::Hasher::new();
    h.update(modernized.as_bytes());
    h.update(&[match stage {
        Stage::Vertex => 0u8,
        Stage::Fragment => 1u8,
    }]);
    h.update(&crate::TRANSLATOR_VERSION.to_le_bytes());
    h.update(&CACHE_FORMAT.to_le_bytes());
    h.finalize().to_hex().to_string()
}

fn cache_load_module(key: &str) -> Option<naga::Module> {
    let bytes = std::fs::read(spirv_cache_dir()?.join(format!("{key}.nga"))).ok()?;
    bincode::deserialize(&bytes).ok()
}

fn cache_store_module(key: &str, module: &naga::Module) {
    if let (Ok(bytes), Some(dir)) = (bincode::serialize(module), spirv_cache_dir()) {
        write_cache_atomic(&dir.join(format!("{key}.nga")), &bytes);
        maybe_prune_cache(&dir);
    }
}

const SHADER_CACHE_CAP_BYTES: u64 = 128 * 1024 * 1024;

fn maybe_prune_cache(dir: &std::path::Path) {
    use std::sync::{Mutex, OnceLock};
    static SWEPT: OnceLock<Mutex<std::collections::HashSet<std::path::PathBuf>>> = OnceLock::new();
    let swept = SWEPT.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let Ok(mut guard) = swept.lock() else { return };
    if !guard.insert(dir.to_path_buf()) {
        return;
    }
    drop(guard);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "nga" || x == "tng"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.len(), m.modified().ok()?))
        })
        .collect();
    let mut remaining: u64 = files.iter().map(|f| f.1).sum();
    if remaining <= SHADER_CACHE_CAP_BYTES {
        return;
    }
    files.sort_by_key(|f| f.2);
    for (path, len, _) in files {
        if remaining <= SHADER_CACHE_CAP_BYTES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining -= len;
        }
    }
}

thread_local! {
    static CACHE_DIR_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

pub fn set_cache_dir(dir: Option<std::path::PathBuf>) {
    CACHE_DIR_OVERRIDE.with(|c| *c.borrow_mut() = dir);
}

#[must_use]
pub fn cache_dir() -> Option<std::path::PathBuf> {
    CACHE_DIR_OVERRIDE.with(|c| c.borrow().clone())
}

fn spirv_cache_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = CACHE_DIR_OVERRIDE.with(|c| c.borrow().clone()) {
        return Some(dir);
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    Some(base.join("kirie").join("shaders"))
}

fn write_cache_atomic(path: &std::path::Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    let tmp = dir.join(format!(".tmp-{}-{stem}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn first_error_line(msg: &str) -> String {
    msg.lines()
        .find(|l| l.contains(": error:"))
        .unwrap_or_else(|| msg.lines().last().unwrap_or(msg))
        .trim()
        .to_string()
}

struct ArrayVarying {
    name: String,
    ty: String,
    count: usize,
    is_out: bool,
    dynamic: bool,
}

fn flatten_array_varyings(src: &str) -> String {
    let arrays = discover_array_varyings(src);
    if arrays.is_empty() {
        return src.to_string();
    }

    let mut out = String::with_capacity(src.len() + 256);
    'lines: for line in src.lines() {
        let t = line.trim();
        for a in &arrays {
            let pat = format!("{}[{}]", a.name, a.count);
            if (t.starts_with("in ") || t.starts_with("out ")) && t.contains(&pat) {
                let kw = if t.starts_with("in ") { "in" } else { "out" };
                for i in 0..a.count {
                    out.push_str(&format!("{kw} {} {}_{i};\n", a.ty, a.name));
                }
                continue 'lines;
            }
        }
        out.push_str(line);
        out.push('\n');
    }

    let mut result = out;
    for a in &arrays {
        if a.dynamic {
            continue;
        }
        for i in 0..a.count {
            result = result.replace(&format!("{}[{i}]", a.name), &format!("{}_{i}", a.name));
        }
    }

    let dyn_arrays: Vec<&ArrayVarying> = arrays.iter().filter(|a| a.dynamic).collect();
    if dyn_arrays.is_empty() {
        return result;
    }
    reconstruct_dynamic_arrays(&result, &dyn_arrays)
}

fn discover_array_varyings(src: &str) -> Vec<ArrayVarying> {
    let mut arrays: Vec<ArrayVarying> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        let (is_out, rest) = if let Some(r) = t.strip_prefix("in ") {
            (false, r)
        } else if let Some(r) = t.strip_prefix("out ") {
            (true, r)
        } else {
            continue;
        };
        let Some(decl) = rest.strip_suffix(';') else {
            continue;
        };
        let Some(open) = decl.find('[') else { continue };
        let Some(close) = decl.find(']') else { continue };
        if close <= open {
            continue;
        }
        let count: usize = decl[open + 1..close].trim().parse().unwrap_or(0);
        let head = decl[..open].trim();
        let mut parts = head.split_whitespace();
        let (Some(ty), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let dynamic = array_has_dynamic_index(src, name);
        arrays.push(ArrayVarying {
            name: name.to_string(),
            ty: ty.to_string(),
            count,
            is_out,
            dynamic,
        });
    }
    arrays
}

fn array_has_dynamic_index(src: &str, name: &str) -> bool {
    let bytes = src.as_bytes();
    let mut search = 0;
    while let Some(rel) = src[search..].find(name) {
        let start = search + rel;
        let end = start + name.len();
        search = end;
        let before_ok = start
            .checked_sub(1)
            .map(|b| !is_ident_byte(bytes[b]))
            .unwrap_or(true);
        if !before_ok || bytes.get(end) != Some(&b'[') {
            continue;
        }
        let Some(close_rel) = src[end + 1..].find(']') else {
            continue;
        };
        let inner = src[end + 1..end + 1 + close_rel].trim();
        if inner.is_empty() {
            continue;
        }
        if !inner.bytes().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn reconstruct_dynamic_arrays(src: &str, arrays: &[&ArrayVarying]) -> String {
    let Some((body_open, body_close)) = main_brace_span(src) else {
        return src.to_string();
    };

    let mut decls = String::new();
    let mut copy_in = String::new();
    let mut copy_out = String::new();
    for a in arrays {
        decls.push_str(&format!("{} {}[{}];\n", a.ty, a.name, a.count));
        if a.is_out {
            for i in 0..a.count {
                copy_out.push_str(&format!("{}_{i} = {}[{i}];\n", a.name, a.name));
            }
        } else {
            for i in 0..a.count {
                copy_in.push_str(&format!("{}[{i}] = {}_{i};\n", a.name, a.name));
            }
        }
    }

    let mut s = String::with_capacity(src.len() + decls.len() + copy_in.len() + copy_out.len() + 4);
    s.push_str(&src[..body_open]);
    s.push('\n');
    s.push_str(&decls);
    s.push_str(&copy_in);
    s.push_str(&src[body_open..body_close]);
    s.push_str(&copy_out);
    s.push_str(&src[body_close..]);
    s
}

fn main_brace_span(src: &str) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut search = 0;
    let main_at = loop {
        let rel = src[search..].find("main")?;
        let start = search + rel;
        let end = start + 4;
        search = end;
        let before_ok = start
            .checked_sub(1)
            .map(|b| !is_ident_byte(bytes[b]))
            .unwrap_or(true);
        let after_paren = src[end..].trim_start().starts_with('(');
        if before_ok && after_paren {
            break start;
        }
    };
    let open = src[main_at..].find('{')? + main_at;
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}
