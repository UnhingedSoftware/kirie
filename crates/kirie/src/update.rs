use anyhow::{Context, Result, anyhow, bail};

const RELEASES: &str = "https://api.github.com/repos/UnhingedSoftware/kirie/releases/latest";

const fn asset_name() -> &'static str {
    if cfg!(feature = "web-cef") {
        "kirie-web-cef-linux-x86_64"
    } else if cfg!(any(feature = "web-webview", feature = "web-webview-inproc")) {
        "kirie-web-webview-linux-x86_64"
    } else {
        "kirie-linux-x86_64"
    }
}

pub fn run(check_only: bool, force: bool) -> Result<()> {
    let current = installed_version();
    let feed = fetch_text(RELEASES).context("could not read the release feed")?;
    let tag = json_string(&feed, "tag_name").ok_or_else(|| anyhow!("the release feed carried no tag"))?;

    println!("installed {current}, latest {tag}");
    if tag == current {
        println!("already up to date");
        return Ok(());
    }
    if check_only {
        println!("run `kirie update` to install it");
        return Ok(());
    }
    if !force && option_env!("KIRIE_RELEASE_TAG").is_none() {
        bail!(
            "this is a local build, not a release — `kirie update` would replace it with {tag}.\n\
             Reinstall from the repo instead, or pass --force to take the release anyway."
        );
    }

    let exe = std::env::current_exe().context("could not find this binary")?;
    let asset = asset_name();
    let url = asset_url(&feed, asset)
        .ok_or_else(|| anyhow!("release {tag} has no {asset} — nothing matching this build"))?;

    println!("downloading {asset}…");
    replace(&exe, &url).with_context(|| format!("could not replace {}", exe.display()))?;

    println!("updated to {tag}");
    println!("restart the engine to run it (kirie.sh --restart, or log out and in)");
    Ok(())
}

fn installed_version() -> String {
    option_env!("KIRIE_RELEASE_TAG").map_or_else(
        || format!("a local build (v{})", env!("CARGO_PKG_VERSION")),
        ToOwned::to_owned,
    )
}

fn replace(path: &std::path::Path, url: &str) -> Result<()> {
    let staged = path.with_extension("update");
    let status = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "600",
            "--output",
        ])
        .arg(&staged)
        .arg(url)
        .status()
        .context("could not run curl")?;
    if !status.success() {
        let _ = std::fs::remove_file(&staged);
        bail!("download failed ({status})");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("could not mark the download executable")?;
    }
    std::fs::rename(&staged, path).map_err(|err| {
        let _ = std::fs::remove_file(&staged);
        anyhow!("{err}")
    })?;
    Ok(())
}

fn fetch_text(url: &str) -> Result<String> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "30",
            "--header",
            "Accept: application/vnd.github+json",
            url,
        ])
        .output()
        .context("could not run curl")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn asset_url(feed: &str, asset: &str) -> Option<String> {
    let needle = format!("\"name\":\"{asset}\"");
    let compact: String = feed.chars().filter(|c| !c.is_whitespace()).collect();
    let at = compact.find(&needle)?;
    let rest = &compact[at..];
    let url_at = rest.find("\"browser_download_url\":\"")? + "\"browser_download_url\":\"".len();
    let tail = &rest[url_at..];
    let end = tail.find('"')?;
    Some(tail[..end].to_owned())
}

fn json_string(feed: &str, key: &str) -> Option<String> {
    let compact: String = feed.chars().filter(|c| !c.is_whitespace()).collect();
    let needle = format!("\"{key}\":\"");
    let at = compact.find(&needle)? + needle.len();
    let tail = &compact[at..];
    let end = tail.find('"')?;
    Some(tail[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"{
      "tag_name": "v0.4.0",
      "assets": [
        {"name":"kirie-linux-x86_64","browser_download_url":"https://example.invalid/plain"},
        {"name":"kirie-web-webview-linux-x86_64","browser_download_url":"https://example.invalid/webview"}
      ]
    }"#;

    #[test]
    fn reads_the_tag_and_the_right_asset() {
        assert_eq!(json_string(FEED, "tag_name").as_deref(), Some("v0.4.0"));
        assert_eq!(
            asset_url(FEED, "kirie-web-webview-linux-x86_64").as_deref(),
            Some("https://example.invalid/webview")
        );
    }

    #[test]
    fn a_missing_asset_is_not_a_wrong_one() {
        assert_eq!(
            asset_url(FEED, "kirie-linux-x86_64").as_deref(),
            Some("https://example.invalid/plain")
        );
        assert!(asset_url(FEED, "kirie-web-cef-linux-x86_64").is_none());
    }

    #[test]
    fn hostile_input_never_panics() {
        for feed in ["", "{", "\"name\":\"kirie-linux-x86_64\"", "{\"tag_name\":\""] {
            let _ = json_string(feed, "tag_name");
            let _ = asset_url(feed, "kirie-linux-x86_64");
        }
    }
}
