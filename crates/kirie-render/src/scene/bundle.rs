use std::path::Path;
use std::time::Instant;

use kirie_bake::{BundleContent, Cache};
use kirie_scene::SceneModel;

const DESCRIPTOR_TAG: &[u8] = b"kirie-scene-bundle-src\x02";

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
}

pub(crate) fn bundle_source(
    pkg_bytes: &[u8],
    project_bytes: Option<&[u8]>,
    assets_dir: Option<&Path>,
) -> Vec<u8> {
    let mut src = Vec::with_capacity(128);
    src.extend_from_slice(DESCRIPTOR_TAG);
    src.extend_from_slice(blake3::hash(pkg_bytes).as_bytes());
    match project_bytes {
        Some(bytes) => {
            src.push(1);
            src.extend_from_slice(blake3::hash(bytes).as_bytes());
        }
        None => src.push(0),
    }
    match assets_dir {
        Some(dir) => {
            src.push(1);
            push_bytes(&mut src, dir.as_os_str().as_encoded_bytes());
        }
        None => src.push(0),
    }
    src
}

pub(crate) fn try_load_model(cache: &Cache, source: &[u8]) -> Option<SceneModel> {
    let start = Instant::now();
    match cache.load(source) {
        Ok(Some(bundle)) => match bundle.scene_model() {
            Ok(model) => {
                tracing::info!(
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    size_bytes = bundle.size_bytes(),
                    "scene bundle hit; skipping parse/resolve/asset load"
                );
                Some(model)
            }
            Err(e) => {
                tracing::warn!(error = %e, "baked scene payload undecodable; evicting bundle");
                let _ = cache.remove(source);
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            tracing::debug!(error = %e, "bundle cache unavailable; loading directly");
            None
        }
    }
}

pub(crate) fn store_model(cache: &Cache, source: &[u8], model: &SceneModel) {
    let start = Instant::now();
    let mut content = BundleContent::new();
    if let Err(e) = content.set_scene_model(model) {
        tracing::warn!(error = %e, "scene model not serializable; skipping bake");
        return;
    }
    match cache.bake(source, content) {
        Ok(path) => tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            path = %path.display(),
            "scene bundle baked"
        ),
        Err(e) => tracing::warn!(error = %e, "scene bundle bake failed"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kirie_scene::{PropertyBag, PropertyValue, Scene};

    use super::*;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p =
                std::env::temp_dir().join(format!("kirie-render-bundle-{}-{tag}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn synthetic_scene_json() -> &'static [u8] {
        br#"{
            "camera": { "eye": "0 0 1", "center": "0 0 0", "up": "0 1 0" },
            "general": { "clearcolor": "0.1 0.2 0.3" },
            "objects": [
                {
                    "image": "models/test.json",
                    "origin": "1 2 3",
                    "visible": { "value": true, "user": "show" }
                }
            ]
        }"#
    }

    fn asset_source(path: &str) -> Option<Vec<u8>> {
        match path {
            "models/test.json" => Some(br#"{ "material": "materials/test.json" }"#.to_vec()),
            "materials/test.json" => Some(
                br#"{ "passes": [ { "shader": "genericimage2", "blending": "translucent" } ] }"#.to_vec(),
            ),
            _ => None,
        }
    }

    fn build_direct(bag: &PropertyBag) -> SceneModel {
        let scene = Scene::from_slice(synthetic_scene_json()).expect("synthetic scene parses");
        let mut model = SceneModel::resolve(scene, bag);
        let problems = model.load_assets(&asset_source, bag);
        assert!(problems.is_empty(), "synthetic assets all resolve: {problems:?}");
        model
    }

    #[test]
    fn bundle_roundtrip_equals_direct_load() {
        let tmp = TmpDir::new("roundtrip");
        let cache = Cache::with_root(&tmp.0);

        let mut bag = PropertyBag::new();
        bag.insert("show", PropertyValue::Bool(false));
        let _props = [("show".to_owned(), PropertyValue::Bool(false))];

        let direct = build_direct(&bag);
        assert!(!direct.scene.objects[0].base.visible.value, "binding resolved");
        match &direct.scene.objects[0].kind {
            kirie_scene::object::ObjectKind::Image(img) => {
                assert!(img.model.is_some(), "model file loaded");
                assert!(img.material.is_some(), "material loaded");
            }
            other => panic!("expected image object, got {other:?}"),
        }

        let source = bundle_source(b"pkg-bytes", Some(b"project-bytes"), None);
        store_model(&cache, &source, &direct);
        let baked = try_load_model(&cache, &source).expect("bundle hit after bake");

        assert_eq!(baked, direct, "bundle round-trip is structurally identical");
        assert_eq!(
            serde_json::to_value(&baked).unwrap(),
            serde_json::to_value(&direct).unwrap(),
            "bundle round-trip is JSON-identical"
        );
    }

    #[test]
    fn defaults_bake_plus_reresolve_equals_direct() {
        let tmp = TmpDir::new("reresolve");
        let cache = Cache::with_root(&tmp.0);

        let mut defaults = PropertyBag::new();
        defaults.insert("show", PropertyValue::Bool(true));
        let baked_model = build_direct(&defaults);
        assert!(baked_model.scene.objects[0].base.visible.value, "default visible");
        let source = bundle_source(b"pkg", Some(b"proj"), None);
        store_model(&cache, &source, &baked_model);

        let mut overridden = PropertyBag::new();
        overridden.insert("show", PropertyValue::Bool(false));
        let mut from_bundle = try_load_model(&cache, &source).expect("hit");
        from_bundle.reresolve(&overridden);

        let direct = build_direct(&overridden);
        assert_eq!(from_bundle, direct, "defaults-bake + reresolve == direct resolve");
        assert!(
            !from_bundle.scene.objects[0].base.visible.value,
            "override applied"
        );

        let mut back = from_bundle;
        back.reresolve(&defaults);
        assert_eq!(back, baked_model, "reresolve to defaults round-trips");
    }

    #[test]
    fn assets_dir_identity_is_part_of_the_key() {
        let none = bundle_source(b"p", None, None);
        let a = bundle_source(b"p", None, Some(Path::new("/opt/we/assets")));
        let b = bundle_source(b"p", None, Some(Path::new("/mnt/we/assets")));
        assert_ne!(none, a);
        assert_ne!(a, b);
    }

    #[test]
    fn source_content_is_part_of_the_key() {
        assert_ne!(
            bundle_source(b"pkg-1", Some(b"proj"), None),
            bundle_source(b"pkg-2", Some(b"proj"), None)
        );
        assert_ne!(
            bundle_source(b"pkg", Some(b"proj-1"), None),
            bundle_source(b"pkg", Some(b"proj-2"), None)
        );
        assert_ne!(
            bundle_source(b"pkg", Some(b"proj"), None),
            bundle_source(b"pkg", None, None)
        );
    }
}
