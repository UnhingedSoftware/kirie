use std::ffi::OsString;
use std::path::{Path, PathBuf};

const STEAM_ROOTS: [&str; 4] = [
    ".local/share/Steam",
    ".steam/steam",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    "snap/steam/common/.local/share/Steam",
];

#[must_use]
pub fn libraries() -> Vec<PathBuf> {
    libraries_with(
        std::env::var_os("KIRIE_STEAM_LIBRARY"),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn libraries_with(override_value: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(value) = override_value {
        return std::env::split_paths(&value).filter(|dir| dir.is_dir()).collect();
    }

    let Some(home) = home else {
        return Vec::new();
    };

    fn push(found: &mut Vec<PathBuf>, dir: PathBuf) {
        if dir.is_dir() && !found.contains(&dir) {
            found.push(dir);
        }
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for root in STEAM_ROOTS {
        push(&mut found, home.join(root));
    }

    for install in found.clone() {
        for library in libraryfolders(&install) {
            push(&mut found, library);
        }
    }

    found
}

fn libraryfolders(install: &Path) -> Vec<PathBuf> {
    ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"]
        .iter()
        .filter_map(|rel| std::fs::read_to_string(install.join(rel)).ok())
        .flat_map(|text| parse_libraryfolders(&text))
        .collect()
}

fn parse_libraryfolders(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let mut quoted = line.split('"').skip(1).step_by(2);
            let key = quoted.next()?;
            if key != "path" && !key.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let value = quoted.next()?;
            if value.is_empty() {
                return None;
            }
            Some(PathBuf::from(value.replace("\\\\", "\\")))
        })
        .collect()
}

#[must_use]
pub fn steamapps_dirs(relative: impl AsRef<Path>) -> Vec<PathBuf> {
    let relative = relative.as_ref();
    libraries()
        .into_iter()
        .map(|library| library.join("steamapps").join(relative))
        .filter(|dir| dir.is_dir())
        .collect()
}

#[must_use]
pub fn steamapps_candidates(relative: impl AsRef<Path>) -> Vec<PathBuf> {
    let relative = relative.as_ref();
    libraries()
        .into_iter()
        .map(|library| library.join("steamapps").join(relative))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_libraryfolders() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/user/.local/share/Steam"
		"label"		""
		"contentid"		"1234567890"
	}
	"1"
	{
		"path"		"/mnt/games/SteamLibrary"
		"label"		""
	}
}
"#;
        assert_eq!(
            parse_libraryfolders(vdf),
            vec![
                PathBuf::from("/home/user/.local/share/Steam"),
                PathBuf::from("/mnt/games/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn parses_legacy_numbered_entries() {
        let vdf = r#"
"LibraryFolders"
{
	"TimeNextStatsReport"		"1234567890"
	"ContentStatsID"		"-1234567890123456789"
	"1"		"/mnt/games/SteamLibrary"
	"2"		"/run/media/user/disk/SteamLibrary"
}
"#;
        assert_eq!(
            parse_libraryfolders(vdf),
            vec![
                PathBuf::from("/mnt/games/SteamLibrary"),
                PathBuf::from("/run/media/user/disk/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn ignores_empty_and_unrelated_keys() {
        let vdf = "\"label\"\t\t\"\"\n\"path\"\t\t\"\"\n\"apps\"\n{\n}\n";
        assert!(parse_libraryfolders(vdf).is_empty());
    }

    #[test]
    fn override_wins_and_keeps_only_existing_dirs() {
        let tmp = std::env::temp_dir().join("kirie-steam-override-test");
        std::fs::create_dir_all(&tmp).expect("temp dir");

        let value =
            std::env::join_paths([tmp.as_path(), Path::new("/definitely/not/here")]).expect("join paths");
        let found = libraries_with(Some(value), Some(PathBuf::from("/nonexistent-home")));

        assert_eq!(found, vec![tmp.clone()]);
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn finds_the_library_a_libraryfolders_points_at() {
        let root = std::env::temp_dir().join("kirie-steam-discovery-test");
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let install = home.join(".local/share/Steam");
        let other_disk = root.join("mnt/games/SteamLibrary");
        std::fs::create_dir_all(install.join("steamapps")).expect("install");
        std::fs::create_dir_all(other_disk.join("steamapps")).expect("library");
        std::fs::write(
            install.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                other_disk.display()
            ),
        )
        .expect("vdf");

        let found = libraries_with(None, Some(home));

        assert!(found.contains(&install), "install itself: {found:?}");
        assert!(found.contains(&other_disk), "second disk: {found:?}");

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkshopItemState {
    pub id: String,
    pub installed: bool,
    pub subscribed: bool,
    pub size: Option<u64>,
    pub updated: Option<u64>,
    pub update_available: bool,
}

#[must_use]
pub fn workshop_item_states(app: &str) -> Vec<WorkshopItemState> {
    let mut out: Vec<WorkshopItemState> = Vec::new();
    for library in libraries() {
        let path = library
            .join("steamapps/workshop")
            .join(format!("appworkshop_{app}.acf"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for state in parse_workshop_acf(&text) {
            if !out.iter().any(|existing| existing.id == state.id) {
                out.push(state);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn parse_workshop_acf(text: &str) -> Vec<WorkshopItemState> {
    let installed = acf_section(text, "WorkshopItemsInstalled");
    let details = acf_section(text, "WorkshopItemDetails");

    let mut ids: Vec<&str> = installed.iter().map(|(id, _)| *id).collect();
    for (id, _) in &details {
        if !ids.contains(id) {
            ids.push(id);
        }
    }

    ids.into_iter()
        .map(|id| {
            let inst = installed.iter().find(|(k, _)| *k == id).map(|(_, v)| v);
            let det = details.iter().find(|(k, _)| *k == id).map(|(_, v)| v);
            let manifest = inst.and_then(|b| acf_value(b, "manifest"));
            let latest = det.and_then(|b| acf_value(b, "latest_manifest"));
            WorkshopItemState {
                id: id.to_owned(),
                installed: inst.is_some(),
                subscribed: det.is_some(),
                size: inst
                    .and_then(|b| acf_value(b, "size"))
                    .and_then(|v| v.parse().ok()),
                updated: inst
                    .and_then(|b| acf_value(b, "timeupdated"))
                    .and_then(|v| v.parse().ok()),
                update_available: match (manifest, latest) {
                    (Some(have), Some(newest)) => have != newest,
                    _ => false,
                },
            }
        })
        .collect()
}

fn acf_section<'a>(text: &'a str, section: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let Some(start) = find_section_body(text, section) else {
        return out;
    };
    let body = &text[start..];

    let mut rest = body;
    while let Some((key, after_key)) = next_quoted(rest) {
        let Some(brace) = after_key.find(|c: char| !c.is_whitespace()) else {
            break;
        };
        if after_key.as_bytes().get(brace) != Some(&b'{') {
            break;
        }
        let block_start = brace + 1;
        let mut idx = block_start;
        let mut depth = 1i32;
        for (i, b) in after_key.as_bytes()[block_start..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        idx = block_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            break;
        }
        out.push((key, &after_key[block_start..idx]));
        rest = &after_key[idx + 1..];

        if rest.trim_start().starts_with('}') {
            break;
        }
    }
    out
}

fn find_section_body(text: &str, section: &str) -> Option<usize> {
    let needle = format!("\"{section}\"");
    let at = text.find(&needle)? + needle.len();
    let brace = text[at..].find('{')?;
    Some(at + brace + 1)
}

fn acf_value<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let at = block.find(&needle)? + needle.len();
    next_quoted(&block[at..]).map(|(value, _)| value)
}

fn next_quoted(text: &str) -> Option<(&str, &str)> {
    let open = text.find('"')?;
    let rest = &text[open + 1..];
    let close = rest.find('"')?;
    Some((&rest[..close], &rest[close + 1..]))
}

#[cfg(test)]
mod acf_tests {
    use super::*;

    const SAMPLE: &str = r#"
"AppWorkshop"
{
	"appid"		"431960"
	"SizeOnDisk"		"3794410365"
	"WorkshopItemsInstalled"
	{
		"1388331347"
		{
			"size"		"4275510"
			"timeupdated"		"1526612969"
			"manifest"		"1182392897106109395"
		}
		"1627026721"
		{
			"size"		"2602171"
			"timeupdated"		"1547964330"
			"manifest"		"6265639993187744802"
		}
	}
	"WorkshopItemDetails"
	{
		"1388331347"
		{
			"manifest"		"1182392897106109395"
			"timeupdated"		"1526612969"
			"subscribedby"		"200304480"
			"latest_manifest"		"1182392897106109395"
		}
		"1627026721"
		{
			"manifest"		"6265639993187744802"
			"latest_manifest"		"9999999999999999999"
		}
		"3600453929"
		{
			"manifest"		"4042593478675097175"
			"latest_manifest"		"4042593478675097175"
		}
	}
}
"#;

    #[test]
    fn reads_installed_subscribed_and_pending_updates() {
        let states = parse_workshop_acf(SAMPLE);
        assert_eq!(states.len(), 3, "two installed plus one subscribed-only");

        let first = states.iter().find(|s| s.id == "1388331347").expect("first");
        assert!(first.installed && first.subscribed);
        assert_eq!(first.size, Some(4_275_510));
        assert_eq!(first.updated, Some(1_526_612_969));
        assert!(!first.update_available, "manifests match");

        let stale = states.iter().find(|s| s.id == "1627026721").expect("stale");
        assert!(stale.update_available, "latest_manifest differs");

        let pending = states.iter().find(|s| s.id == "3600453929").expect("pending");
        assert!(pending.subscribed && !pending.installed);
        assert_eq!(pending.size, None);
    }

    #[test]
    fn malformed_input_yields_no_entries() {
        for text in [
            "",
            "\"AppWorkshop\"",
            "\"AppWorkshop\" {",
            "\"WorkshopItemsInstalled\" { \"123\" { \"size\"",
            "\"WorkshopItemsInstalled\" { \"123\" }",
            "{{{{{{",
            "\"WorkshopItemsInstalled\" { \"123\" { } ",
        ] {
            let _ = parse_workshop_acf(text);
        }
    }
}
