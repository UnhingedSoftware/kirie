use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DIRECTORY: &str = "kirie/properties";

#[must_use]
pub fn store() -> Option<PathBuf> {
    if let Some(set) = std::env::var_os("KIRIE_PROPERTY_STORE") {
        return Some(PathBuf::from(set));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join(DIRECTORY))
}

#[must_use]
pub fn name_for(background: &Path) -> String {
    let last = background
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()) {
        return last;
    }
    let full = background.to_string_lossy();
    let mut out = String::with_capacity(full.len() + last.len() + 1);
    if !last.is_empty() {
        out.push_str(&sanitize(&last));
        out.push('-');
    }
    out.push_str(&short_hash(full.as_bytes()));
    out
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(48)
        .collect()
}

fn short_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[must_use]
pub fn file_for(background: &Path) -> Option<PathBuf> {
    Some(file_in(&store()?, background))
}

#[must_use]
fn file_in(store: &Path, background: &Path) -> PathBuf {
    store.join(format!("{}.json", name_for(background)))
}

#[must_use]
pub fn read(background: &Path) -> Vec<(String, String)> {
    let Some(path) = file_for(background) else {
        return Vec::new();
    };
    read_file(&path)
}

#[must_use]
fn read_in(store: &Path, background: &Path) -> Vec<(String, String)> {
    read_file(&file_in(store, background))
}

#[must_use]
fn read_file(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<BTreeMap<String, String>>(&text) {
        Ok(map) => map.into_iter().collect(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "saved properties are unreadable");
            Vec::new()
        }
    }
}

pub fn write(background: &Path, properties: &[(String, String)]) {
    let Some(path) = file_for(background) else {
        return;
    };
    write_file(&path, properties);
}

fn write_in(store: &Path, background: &Path, properties: &[(String, String)]) {
    write_file(&file_in(store, background), properties);
}

fn write_file(path: &Path, properties: &[(String, String)]) {
    if properties.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let map: BTreeMap<&str, &str> = properties
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let Ok(text) = serde_json::to_string_pretty(&map) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = %parent.display(), %error, "cannot make the property store");
        return;
    }
    if let Err(error) = std::fs::write(path, text) {
        tracing::warn!(path = %path.display(), %error, "cannot save the properties");
    }
}

pub fn remember(background: &Path, key: &str, value: &str) {
    let Some(store) = store() else { return };
    remember_in(&store, background, key, value);
}

fn remember_in(store: &Path, background: &Path, key: &str, value: &str) {
    let mut saved = read_in(store, background);
    match saved.iter_mut().find(|(had, _)| had == key) {
        Some(slot) => slot.1 = value.to_owned(),
        None => saved.push((key.to_owned(), value.to_owned())),
    }
    write_in(store, background, &saved);
}

#[must_use]
pub fn with_saved(background: &Path, asked: &[(String, String)]) -> Vec<(String, String)> {
    merge(read(background), asked)
}

#[must_use]
fn merge(saved: Vec<(String, String)>, asked: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = saved;
    for (key, value) in asked {
        match out.iter_mut().find(|(had, _)| had == key) {
            Some(slot) => slot.1.clone_from(value),
            None => out.push((key.clone(), value.clone())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Store(PathBuf);

    impl Store {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kirie-props-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_workshop_item_is_stored_under_its_id() {
        assert_eq!(
            name_for(Path::new("/home/a/Steam/steamapps/workshop/content/431960/3299228616")),
            "3299228616"
        );
    }

    #[test]
    fn two_wallpapers_with_the_same_folder_name_do_not_share_a_file() {
        let one = name_for(Path::new("/wallpapers/rain"));
        let two = name_for(Path::new("/elsewhere/rain"));
        assert!(one.starts_with("rain-"), "{one}");
        assert_ne!(one, two);
    }

    #[test]
    fn what_is_written_comes_back() {
        let store = Store::new("roundtrip");
        let bg = Path::new("/wallpapers/431960/12345");
        write_in(store.path(), bg, &[("bloom".to_owned(), "1".to_owned())]);
        assert_eq!(
            read_in(store.path(), bg),
            vec![("bloom".to_owned(), "1".to_owned())]
        );
    }

    #[test]
    fn remembering_a_key_leaves_the_others_alone() {
        let store = Store::new("remember");
        let bg = Path::new("/wallpapers/431960/222");
        remember_in(store.path(), bg, "bloom", "1");
        remember_in(store.path(), bg, "fov", "34");
        remember_in(store.path(), bg, "bloom", "0");

        let saved = read_in(store.path(), bg);
        assert_eq!(saved.len(), 2, "{saved:?}");
        assert!(saved.contains(&("bloom".to_owned(), "0".to_owned())), "{saved:?}");
        assert!(saved.contains(&("fov".to_owned(), "34".to_owned())), "{saved:?}");
    }

    #[test]
    fn an_asked_for_value_wins_over_the_saved_one() {
        let saved = vec![
            ("bloom".to_owned(), "1".to_owned()),
            ("fov".to_owned(), "34".to_owned()),
        ];
        let merged = merge(saved, &[("bloom".to_owned(), "0".to_owned())]);
        assert!(merged.contains(&("bloom".to_owned(), "0".to_owned())), "{merged:?}");
        assert!(merged.contains(&("fov".to_owned(), "34".to_owned())), "{merged:?}");
    }

    #[test]
    fn a_wallpaper_with_nothing_saved_reads_as_empty() {
        let store = Store::new("empty");
        assert!(read_in(store.path(), Path::new("/wallpapers/431960/444")).is_empty());
    }

    #[test]
    fn saving_nothing_removes_the_file() {
        let store = Store::new("clear");
        let bg = Path::new("/wallpapers/431960/555");
        write_in(store.path(), bg, &[("bloom".to_owned(), "1".to_owned())]);
        write_in(store.path(), bg, &[]);
        assert!(read_in(store.path(), bg).is_empty());
        assert!(!file_in(store.path(), bg).exists());
    }

    #[test]
    fn the_store_lives_under_the_config_home() {
        let Some(path) = store() else { return };
        assert!(path.ends_with("kirie/properties"), "{}", path.display());
    }
}
