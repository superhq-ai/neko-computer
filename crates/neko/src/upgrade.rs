use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use serde::Deserialize;
use tar::Archive;

use crate::dist::REPO;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// Replace the running neko binary with the latest release for this platform.
pub async fn upgrade() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let platform = platform()?;

    let tag = latest_tag().await?;
    let latest = tag.trim_start_matches('v');
    if latest == current {
        println!("neko is up to date ({current})");
        return Ok(());
    }

    println!("upgrading neko {current} -> {latest}");
    let tarball = format!("neko-{tag}-{platform}.tar.gz");
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{tarball}");
    let bytes = crate::github::get(&url)
        .send()
        .await
        .and_then(crate::github::explain)
        .with_context(|| format!("downloading {tarball}"))?
        .bytes()
        .await?;

    let exe = std::env::current_exe().context("locating the running neko binary")?;
    let staged = exe.with_file_name(".neko.upgrade");

    let staged_for_task = staged.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut archive = Archive::new(GzDecoder::new(&bytes[..]));
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path()?.file_name().and_then(|n| n.to_str()) == Some("neko") {
                entry.unpack(&staged_for_task)?;
                return Ok(());
            }
        }
        bail!("release tarball did not contain a neko binary")
    })
    .await??;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staged, &exe).with_context(|| format!("replacing {}", exe.display()))?;
    println!("neko is now {latest}");
    Ok(())
}

async fn latest_tag() -> Result<String> {
    let release: Release = crate::github::get(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
    .header("accept", "application/vnd.github+json")
    .send()
    .await
    .and_then(crate::github::explain)
    .context("checking the latest release")?
    .json()
    .await?;
    Ok(release.tag_name)
}

fn platform() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        (os, arch) => Err(anyhow!("no prebuilt neko for {os} {arch}")),
    }
}
