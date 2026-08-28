//! `kirie assets` — where Wallpaper Engine's shared assets are, if anywhere.
//!
//! A shell needs this to decide whether to offer its Wallpaper Engine surface
//! at all: kirie being installed says nothing about Wallpaper Engine being
//! installed, and every scene wallpaper needs those assets. `kirie check`
//! reports it too, but as prose and after probing GPUs — too heavy for a panel
//! to ask at startup.

use anyhow::Result;

use crate::compat::resolve;

/// Run `kirie assets`.
///
/// `--json` always succeeds: the answer is in the payload, and a caller that
/// cannot parse the output learns something useful from that — an engine too
/// old to know this subcommand treats `assets` as a wallpaper path and says so
/// in prose. Without `--json` the exit status carries the answer, so a script
/// can branch on it alone.
///
/// # Errors
/// Only a write failure to stdout.
pub fn run(json: bool) -> Result<bool> {
    let dir = resolve::we_assets_dir();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "assets": dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
                "installed": dir.is_some(),
            })
        );
    } else if let Some(dir) = &dir {
        println!("{}", dir.display());
    } else {
        println!("no Wallpaper Engine assets found");
        println!("(install Wallpaper Engine via Steam, or set KIRIE_WE_ASSETS)");
    }

    Ok(json || dir.is_some())
}
