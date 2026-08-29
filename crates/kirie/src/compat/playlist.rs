#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::compat::args::ParseError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSettings {
    pub delay_minutes: u32,
    pub mode: String,
    pub order: String,
    pub update_on_pause: bool,
    pub video_sequence: bool,
}

impl Default for PlaylistSettings {
    fn default() -> Self {
        Self {
            delay_minutes: 60,
            mode: "timer".to_owned(),
            order: "sequential".to_owned(),
            update_on_pause: false,
            video_sequence: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistDefinition {
    pub name: String,
    pub items: Vec<PathBuf>,
    pub settings: PlaylistSettings,
}

const APP_DIRECTORY_RELATIVE: &str = "common/wallpaper_engine";

fn fatal(message: impl Into<String>) -> ParseError {
    ParseError {
        message: message.into(),
        doubled: true,
    }
}

fn config_file_path() -> Result<PathBuf, ParseError> {
    crate::compat::steam::steamapps_dirs(APP_DIRECTORY_RELATIVE)
        .into_iter()
        .next()
        .map(|dir| dir.join("config.json"))
        .ok_or_else(|| fatal("Cannot locate wallpaper engine installation to read config.json"))
}

pub fn load_config_playlists() -> Result<BTreeMap<String, PlaylistDefinition>, ParseError> {
    load_playlists_from(&config_file_path()?)
}

pub fn load_playlists_from(path: &Path) -> Result<BTreeMap<String, PlaylistDefinition>, ParseError> {
    let text = std::fs::read_to_string(path).map_err(|_| {
        fatal(format!(
            "Cannot open wallpaper engine config file at {}",
            path.display()
        ))
    })?;
    let root: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Failed parsing wallpaper engine config.json: {err}");
            return Ok(BTreeMap::new());
        }
    };
    let Some(steam_user) = root.get("steamuser").filter(|v| !v.is_null()) else {
        return Err(fatal("Cannot find steamuser section in config.json"));
    };

    let mut map = BTreeMap::new();

    if let Some(playlists) = steam_user
        .get("general")
        .and_then(|g| g.get("playlists"))
        .and_then(Value::as_array)
    {
        for playlist in playlists {
            let fallback = coerce_string(playlist.get("name"), "");
            if let Some(def) = build_definition(playlist, &fallback) {
                map.insert(def.name.clone(), def);
            }
        }
    }

    if let Some(selected) = steam_user
        .get("wallpaperconfig")
        .and_then(|w| w.get("selectedwallpapers"))
        .and_then(Value::as_object)
    {
        for (key, entry) in selected {
            let Some(playlist) = entry.get("playlist").filter(|v| !v.is_null()) else {
                continue;
            };
            if let Some(def) = build_definition(playlist, key) {
                map.insert(def.name.clone(), def);
            }
        }
    }

    Ok(map)
}

pub fn get<'m>(
    map: &'m BTreeMap<String, PlaylistDefinition>,
    name: &str,
) -> Result<&'m PlaylistDefinition, ParseError> {
    if let Some(def) = map.get(name) {
        return Ok(def);
    }
    let available = map.keys().cloned().collect::<Vec<_>>().join(", ");
    let suffix = if available.is_empty() {
        String::new()
    } else {
        format!(". Available: {available}")
    };
    Err(fatal(format!(
        "Playlist not found in config.json: {name}{suffix}"
    )))
}

fn build_definition(json: &Value, fallback: &str) -> Option<PlaylistDefinition> {
    let mut name = coerce_string(json.get("name"), fallback);
    let settings = parse_settings(json);
    let items = collect_items(json, if name.is_empty() { fallback } else { &name });
    if items.is_empty() {
        return None;
    }
    if name.is_empty() {
        if fallback.is_empty() {
            eprintln!("Skipping playlist with no name");
            return None;
        }
        name = fallback.to_owned();
    }
    Some(PlaylistDefinition {
        name,
        items,
        settings,
    })
}

fn parse_settings(json: &Value) -> PlaylistSettings {
    let s = json.get("settings").filter(|v| !v.is_null());
    let field = |key: &str| s.and_then(|s| s.get(key)).filter(|v| !v.is_null());
    PlaylistSettings {
        delay_minutes: coerce_u32(field("delay"), 60),
        mode: coerce_string(field("mode"), "timer"),
        order: coerce_string(field("order"), "sequential"),
        update_on_pause: coerce_bool(field("updateonpause"), false),
        video_sequence: coerce_bool(field("videosequence"), false),
    }
}

fn collect_items(json: &Value, name: &str) -> Vec<PathBuf> {
    let Some(items) = json.get("items").and_then(Value::as_array) else {
        eprintln!("Skipping playlist {name}: missing items");
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in items {
        let Some(raw) = raw.as_str() else { continue };
        let Some(resolved) = resolve_item_path(raw) else {
            continue;
        };
        if !resolved.exists() {
            eprintln!("Skipping playlist item not found: {}", resolved.display());
            continue;
        }
        out.push(resolved);
    }
    if out.is_empty() {
        eprintln!("Skipping playlist {name}: no usable items found");
    }
    out
}

fn resolve_item_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let mut cleaned = raw.strip_prefix("\\\\?\\").unwrap_or(raw).replace('\\', "/");
    if cleaned.len() > 1 && cleaned.as_bytes()[1] == b':' {
        cleaned = cleaned[2..].to_owned();
    }
    if !cleaned.is_empty() && !cleaned.starts_with('/') {
        cleaned.insert(0, '/');
    }
    let mut path = lexically_normal(Path::new(&cleaned));
    if path.as_os_str().is_empty() {
        return None;
    }
    if path.is_file()
        && let Some(parent) = path.parent()
    {
        path = parent.to_path_buf();
    }
    Some(path)
}

fn lexically_normal(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::RootDir => out.push("/"),
            Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    out
}

fn coerce_u32(v: Option<&Value>, default: u32) -> u32 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|x| u32::try_from(x).unwrap_or(u32::MAX))
            .or_else(|| n.as_f64().map(|f| f as u32))
            .unwrap_or(default),
        Some(Value::Bool(b)) => u32::from(*b),
        Some(Value::String(s)) => leading_u32(s),
        _ => default,
    }
}

fn leading_u32(s: &str) -> u32 {
    let t = s.trim_start();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let end = digits
        .as_bytes()
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(digits.len());
    let v = digits[..end].parse::<u64>().unwrap_or(0);
    if neg {
        0
    } else {
        u32::try_from(v).unwrap_or(u32::MAX)
    }
}

fn coerce_string(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        _ => default.to_owned(),
    }
}

fn coerce_bool(v: Option<&Value>, default: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => matches!(s.as_str(), "1" | "true" | "True" | "TRUE"),
        _ => default,
    }
}

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn seeded() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self(nanos ^ (u64::from(std::process::id()) << 32))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn shuffle(order: &mut [usize], rng: &mut Rng) {
    for i in (1..order.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
}

fn build_order(definition: &PlaylistDefinition, rng: &mut Rng) -> Vec<usize> {
    let mut order: Vec<usize> = (0..definition.items.len()).collect();
    if definition.settings.order == "random" {
        shuffle(&mut order, rng);
    }
    order
}

fn delay_of(definition: &PlaylistDefinition) -> Duration {
    Duration::from_secs(60 * u64::from(definition.settings.delay_minutes.max(1)))
}

pub(crate) struct ActivePlaylist {
    definition: PlaylistDefinition,
    order: Vec<usize>,
    order_index: usize,
    failed: HashSet<usize>,
    next_switch: Instant,
}

impl ActivePlaylist {
    pub(crate) fn start(
        definition: PlaylistDefinition,
        current: Option<&Path>,
        now: Instant,
        rng: &mut Rng,
    ) -> Option<Self> {
        if definition.items.is_empty() {
            return None;
        }
        let order = build_order(&definition, rng);
        if order.is_empty() {
            return None;
        }
        let mut order_index = 0;
        if let Some(current) = current {
            for (i, &item) in order.iter().enumerate() {
                if definition.items[item] == current {
                    order_index = i;
                    break;
                }
            }
        }
        let delay = delay_of(&definition);
        Some(Self {
            definition,
            order,
            order_index,
            failed: HashSet::new(),
            next_switch: now + delay,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.definition.name
    }

    pub(crate) fn item_count(&self) -> usize {
        self.definition.items.len()
    }

    pub(crate) fn due(&self, now: Instant) -> bool {
        self.definition.settings.mode == "timer" && self.definition.items.len() > 1 && now >= self.next_switch
    }

    pub(crate) fn next_due(&self) -> Option<Instant> {
        (self.definition.settings.mode == "timer" && self.definition.items.len() > 1)
            .then_some(self.next_switch)
    }

    fn select_next_candidate(&self, candidate: usize) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        let mut candidate = candidate;
        for _ in 0..self.order.len() {
            if !self.failed.contains(&self.order[candidate]) {
                return Some(candidate);
            }
            candidate = (candidate + 1) % self.order.len();
        }
        None
    }

    pub(crate) fn advance<P, S>(
        &mut self,
        screen: &str,
        now: Instant,
        rng: &mut Rng,
        mut preflight: P,
        mut show: S,
    ) where
        P: FnMut(&Path) -> bool,
        S: FnMut(&Path) -> bool,
    {
        if self.order.is_empty() {
            return;
        }
        self.order_index = (self.order_index + 1) % self.order.len();
        if self.order_index == 0 && self.definition.settings.order == "random" {
            shuffle(&mut self.order, rng);
        }

        let Some(mut candidate) = self.select_next_candidate(self.order_index) else {
            eprintln!("All playlist items failed for {screen}, keeping current wallpaper");
            self.next_switch = now + delay_of(&self.definition);
            return;
        };

        let candidate_item = self.order[candidate];
        if !preflight(&self.definition.items[candidate_item]) {
            self.failed.insert(candidate_item);
            let Some(next) = self.select_next_candidate(candidate) else {
                eprintln!("All playlist items failed for {screen}, keeping current wallpaper");
                self.next_switch = now + delay_of(&self.definition);
                return;
            };
            candidate = next;
        }

        self.order_index = candidate;
        let shown_item = self.order[self.order_index];
        if !show(&self.definition.items[shown_item]) {
            self.failed.insert(shown_item);
            eprintln!("Failed to load wallpaper for {screen}, will retry on next cycle");
        }

        self.next_switch = now + delay_of(&self.definition);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kirie-playlist-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn item_path_munging_matches_reference() {
        assert_eq!(
            resolve_item_path("\\\\?\\C:\\Users\\me\\proj\\123"),
            Some(PathBuf::from("/Users/me/proj/123"))
        );
        assert_eq!(
            resolve_item_path("Z:\\home\\me\\wp\\42"),
            Some(PathBuf::from("/home/me/wp/42"))
        );
        assert_eq!(
            resolve_item_path("foo/./bar/../baz"),
            Some(PathBuf::from("/foo/baz"))
        );
        assert_eq!(resolve_item_path(""), None);
        assert_eq!(resolve_item_path("C:"), None);
    }

    #[test]
    fn item_path_file_resolves_to_parent_dir() {
        let dir = temp_dir("parent");
        let file = dir.join("project.json");
        std::fs::write(&file, "{}").unwrap();
        let raw = file.to_string_lossy().replace('/', "\\");
        assert_eq!(resolve_item_path(&raw), Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_playlists_load_with_reference_semantics() {
        let root = temp_dir("config");
        let item_a = root.join("item-a");
        let item_b = root.join("item-b");
        std::fs::create_dir_all(&item_a).unwrap();
        std::fs::create_dir_all(&item_b).unwrap();
        let config = root.join("config.json");
        let json = serde_json::json!({
            "steamuser": {
                "general": {
                    "playlists": [
                        {
                            "name": "day",
                            "settings": { "delay": "30", "order": "random" },
                            "items": [
                                item_a.to_string_lossy(),
                                "/nonexistent/kirie-test-item"
                            ]
                        },
                        {
                            "name": "broken",
                            "items": ["/nonexistent/kirie-test-item"]
                        }
                    ]
                },
                "wallpaperconfig": {
                    "selectedwallpapers": {
                        "Monitor0": {
                            "playlist": {
                                "name": "night",
                                "items": [item_b.to_string_lossy()]
                            }
                        }
                    }
                }
            }
        });
        std::fs::write(&config, serde_json::to_string(&json).unwrap()).unwrap();

        let map = load_playlists_from(&config).unwrap();
        assert_eq!(map.len(), 2, "broken playlist must be skipped: {map:?}");

        let day = &map["day"];
        assert_eq!(day.items, vec![item_a.clone()]);
        assert_eq!(day.settings.delay_minutes, 30);
        assert_eq!(day.settings.order, "random");
        assert_eq!(day.settings.mode, "timer");

        let night = &map["night"];
        assert_eq!(night.items, vec![item_b.clone()]);
        assert_eq!(night.settings.delay_minutes, 60);

        let err = get(&map, "nope").unwrap_err();
        assert_eq!(
            err.message,
            "Playlist not found in config.json: nope. Available: day, night"
        );
        assert!(err.doubled);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_steamuser_is_fatal_and_bad_json_is_not() {
        let root = temp_dir("badcfg");
        let config = root.join("config.json");

        std::fs::write(&config, "{}").unwrap();
        let err = load_playlists_from(&config).unwrap_err();
        assert_eq!(err.message, "Cannot find steamuser section in config.json");

        std::fs::write(&config, "not json").unwrap();
        assert!(load_playlists_from(&config).unwrap().is_empty());

        let missing = root.join("missing.json");
        let err = load_playlists_from(&missing).unwrap_err();
        assert!(
            err.message
                .starts_with("Cannot open wallpaper engine config file at ")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    fn test_definition(order: &str, delay: u32) -> PlaylistDefinition {
        PlaylistDefinition {
            name: "t".to_owned(),
            items: vec![PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")],
            settings: PlaylistSettings {
                delay_minutes: delay,
                order: order.to_owned(),
                ..PlaylistSettings::default()
            },
        }
    }

    #[test]
    fn sequential_rotation_wraps() {
        let mut rng = Rng(1);
        let now = Instant::now();
        let mut pl = ActivePlaylist::start(
            test_definition("sequential", 60),
            Some(Path::new("/a")),
            now,
            &mut rng,
        )
        .unwrap();

        assert!(!pl.due(now));
        let t1 = now + Duration::from_secs(60 * 60);
        assert!(pl.due(t1));

        let mut shown = Vec::new();
        for i in 0..4 {
            let t = t1 + Duration::from_secs(i * 60 * 60);
            pl.advance(
                "HDMI-A-1",
                t,
                &mut rng,
                |_| true,
                |p| {
                    shown.push(p.to_path_buf());
                    true
                },
            );
        }
        assert_eq!(
            shown,
            vec![
                PathBuf::from("/b"),
                PathBuf::from("/c"),
                PathBuf::from("/a"),
                PathBuf::from("/b"),
            ]
        );
    }

    #[test]
    fn preflight_failure_skips_to_the_next_candidate() {
        let mut rng = Rng(2);
        let now = Instant::now();
        let mut pl = ActivePlaylist::start(
            test_definition("sequential", 60),
            Some(Path::new("/a")),
            now,
            &mut rng,
        )
        .unwrap();

        let mut shown = Vec::new();
        for _ in 0..3 {
            pl.advance(
                "s",
                now,
                &mut rng,
                |p| p != Path::new("/b"),
                |p| {
                    shown.push(p.to_path_buf());
                    true
                },
            );
        }
        assert_eq!(
            shown,
            vec![PathBuf::from("/c"), PathBuf::from("/a"), PathBuf::from("/c"),]
        );
    }

    #[test]
    fn all_items_failed_keeps_the_current_wallpaper() {
        let mut rng = Rng(3);
        let now = Instant::now();
        let mut pl = ActivePlaylist::start(
            test_definition("sequential", 60),
            Some(Path::new("/a")),
            now,
            &mut rng,
        )
        .unwrap();

        let mut calls = 0;
        for _ in 0..6 {
            pl.advance(
                "s",
                now,
                &mut rng,
                |_| true,
                |_| {
                    calls += 1;
                    false
                },
            );
        }
        assert_eq!(calls, 3, "each item fails exactly once, then all are skipped");
        assert!(!pl.due(now), "next switch must be rescheduled");
    }

    #[test]
    fn non_timer_modes_and_single_items_never_rotate() {
        let mut rng = Rng(4);
        let now = Instant::now();
        let far = now + Duration::from_secs(1_000_000);

        let mut def = test_definition("sequential", 1);
        def.settings.mode = "videosequence".to_owned();
        let pl = ActivePlaylist::start(def, None, now, &mut rng).unwrap();
        assert!(!pl.due(far), "only timer mode rotates");

        let mut def = test_definition("sequential", 1);
        def.items.truncate(1);
        let pl = ActivePlaylist::start(def, None, now, &mut rng).unwrap();
        assert!(!pl.due(far), "single-item playlists never rotate");
    }

    #[test]
    fn random_order_is_a_permutation() {
        let mut rng = Rng::seeded();
        let def = test_definition("random", 60);
        let pl = ActivePlaylist::start(def, Some(Path::new("/a")), Instant::now(), &mut rng).unwrap();
        let mut sorted = pl.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2]);
        assert_eq!(pl.order[pl.order_index], 0);
    }
}
