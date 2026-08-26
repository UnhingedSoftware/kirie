//! `kirie update` — replace this binary with the newest release.
//!
//! kirie ships as standalone executables rather than a package, so there is no
//! `pacman -Syu` to carry it along. Without a command like this, staying
//! current means remembering which asset variant you installed, downloading it
//! by hand, and remembering the Steam helper beside it.
//!
//! Three things it is careful about:
//!
//! * **It replaces what it is running as**, resolved through `/proc/self/exe`,
//!   so a `~/.local/bin/kirie` that shadows a `/usr/bin/kirie` is the one that
//!   gets updated — not whichever the shell would find first.
//! * **It keeps the variant.** A build with the webview backend must not be
//!   replaced by the plain one: web wallpapers would silently stop rendering.
//!   The running binary reports its own features, and the matching asset is
//!   the only one downloaded.
//! * **It never leaves half a binary behind.** The download lands beside the
//!   target and is renamed over it, which is atomic on the same filesystem.

use anyhow::{Context, Result, anyhow, bail};

/// Where releases are published.
const RELEASES: &str = "https://api.github.com/repos/UnhingedSoftware/kirie/releases/latest";

/// The asset this build corresponds to.
///
/// Compiled in rather than sniffed at runtime: the binary knows which features
/// it was built with, and guessing from behaviour would be a guess.
const fn asset_name() -> &'static str {
    if cfg!(feature = "web-cef") {
        "kirie-web-cef-linux-x86_64"
    } else if cfg!(any(feature = "web-webview", feature = "web-webview-inproc")) {
        "kirie-web-webview-linux-x86_64"
    } else {
        "kirie-linux-x86_64"
    }
}

/// Run `kirie update`.
///
/// # Errors
/// When the release feed cannot be read, the asset is missing, the download
/// fails, or the installed binary cannot be replaced (a packaged install in
/// `/usr` needs the package manager, not this).
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
        // Replacing someone's own build with a release throws away whatever
        // they built it for. Say so and stop; `--force` is their way of
        // saying they meant it.
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

/// What this binary calls itself when comparing against a release.
///
/// The workspace version is `0.1.0` and always has been — releases are tagged
/// from the changelog, not from `Cargo.toml`, so comparing against the crate
/// version would report every build as out of date forever. The release
/// workflow stamps the tag it built into `KIRIE_RELEASE_TAG`; a local build
/// has no stamp and says so, since "is my own build older than v0.3.0" is not
/// a question a version number can answer.
fn installed_version() -> String {
    option_env!("KIRIE_RELEASE_TAG").map_or_else(
        || format!("a local build (v{})", env!("CARGO_PKG_VERSION")),
        ToOwned::to_owned,
    )
}

/// Download `url` over `path`, atomically.
fn replace(path: &std::path::Path, url: &str) -> Result<()> {
    // Beside the target, so the rename stays on one filesystem — across
    // filesystems it would be a copy, and a copy can be interrupted half-way.
    let staged = path.with_extension("update");
    let status = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            // The URL comes from a network document; keep it to the two
            // protocols that make sense, across redirects too.
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
    // Renaming over a *running* binary is fine on Linux: the kernel keeps the
    // open inode alive for this process, and the next launch gets the new one.
    std::fs::rename(&staged, path).map_err(|err| {
        let _ = std::fs::remove_file(&staged);
        anyhow!("{err}")
    })?;
    Ok(())
}

/// Fetch a URL as text.
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

/// The `browser_download_url` of the asset with this exact name.
///
/// Scanned rather than parsed: pulling in a JSON dependency for two fields of
/// a document GitHub controls is not worth it, and a miss is a clean `None`.
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

/// The value of a top-level string field.
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
        // The plain asset's name is a prefix of the others, so a sloppy search
        // would hand a webview build the wrong file — and web wallpapers would
        // quietly stop working.
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
