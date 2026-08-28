use anyhow::Result;

use crate::compat::resolve;

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
