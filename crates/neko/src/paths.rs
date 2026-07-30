use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use tar::Archive;
use tokio::io::AsyncWriteExt;

use crate::dist::{SHURU_OS_VERSION, SHURU_REPO};

/// neko's own data directory, separate from shuru's, holding the SQLite index,
/// checkpoint images, and the base VM assets.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("NEKO_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow!("no data directory on this platform"))?
        .join("neko"))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("neko.db"))
}

pub fn checkpoint_image(id: &str) -> Result<PathBuf> {
    Ok(data_dir()?.join("checkpoints").join(format!("{id}.ext4")))
}

/// Ensure the data dir carries the pinned base VM image. Already have it: done.
/// A shuru install on the same version is reused (linked in, no copy); otherwise
/// the pinned image is downloaded, so `neko run` needs no separate shuru and
/// picks up a new image whenever the pin moves.
pub async fn ensure_ready() -> Result<PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(dir.join("checkpoints"))?;

    if provisioned(&dir) {
        return Ok(dir);
    }

    let shuru = shuru_sdk::default_data_dir();
    let shuru = Path::new(&shuru);
    if version_of(shuru).as_deref() == Some(SHURU_OS_VERSION) {
        clear_assets(&dir);
        link_from_shuru(shuru, &dir, "Image", Kind::Symlink)?;
        link_from_shuru(shuru, &dir, "rootfs.ext4", Kind::Hardlink)?;
        link_from_shuru(shuru, &dir, "initramfs.cpio.gz", Kind::Symlink)?;
        if assets_present(&dir) {
            std::fs::copy(shuru.join("VERSION"), dir.join("VERSION"))?;
            return Ok(dir);
        }
    }

    clear_assets(&dir);
    download_base_image(&dir).await?;
    Ok(dir)
}

fn assets_present(dir: &Path) -> bool {
    dir.join("Image").exists() && dir.join("rootfs.ext4").exists()
}

fn provisioned(dir: &Path) -> bool {
    version_of(dir).as_deref() == Some(SHURU_OS_VERSION) && assets_present(dir)
}

fn version_of(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("VERSION"))
        .ok()
        .map(|v| v.trim().to_string())
}

fn clear_assets(dir: &Path) {
    for name in ["Image", "rootfs.ext4", "initramfs.cpio.gz", "VERSION"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

enum Kind {
    Symlink,
    Hardlink,
}

fn link_from_shuru(src_dir: &Path, dst_dir: &Path, name: &str, kind: Kind) -> Result<()> {
    let dst = dst_dir.join(name);
    let src = src_dir.join(name);
    if dst.exists() || !src.exists() {
        return Ok(());
    }
    match kind {
        Kind::Symlink => std::os::unix::fs::symlink(&src, &dst),
        Kind::Hardlink => std::fs::hard_link(&src, &dst),
    }
    .with_context(|| format!("linking base asset {name}"))?;
    Ok(())
}

/// Stream the shuru OS image tarball from GitHub releases to a temp file, then
/// extract the kernel, rootfs, and initramfs into the data dir.
async fn download_base_image(dir: &Path) -> Result<()> {
    let tag = format!("v{SHURU_OS_VERSION}");
    let tarball = format!("shuru-os-{tag}-aarch64.tar.gz");
    let url = format!("https://github.com/{SHURU_REPO}/releases/download/{tag}/{tarball}");

    let response = crate::github::get(&url)
        .send()
        .await
        .and_then(crate::github::explain)
        .with_context(|| format!("downloading base image {tag}"))?;
    let total = response.content_length();

    let part = dir.join(format!("{tarball}.part"));
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = response.bytes_stream();
    let mut done: u64 = 0;
    let mut progress = Progress::new(&format!("downloading base image {tag}"), total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress.update(done);
    }
    file.flush().await?;
    drop(file);
    progress.finish(done);

    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&part)?;
        Archive::new(GzDecoder::new(std::io::BufReader::new(file)))
            .unpack(&dir)
            .context("extracting base image")?;
        std::fs::write(dir.join("VERSION"), format!("{SHURU_OS_VERSION}\n"))?;
        let _ = std::fs::remove_file(&part);
        Ok(())
    })
    .await??;
    Ok(())
}

/// Single-line, in-place download progress on stderr. Redraws with a carriage
/// return on a TTY; falls back to occasional line-per-decile output when stderr
/// is piped so logs stay readable.
struct Progress {
    label: String,
    total: Option<u64>,
    tty: bool,
    last_decile: u64,
}

impl Progress {
    fn new(label: &str, total: Option<u64>) -> Self {
        use std::io::IsTerminal;
        let p = Self {
            label: label.to_string(),
            total,
            tty: std::io::stderr().is_terminal(),
            last_decile: u64::MAX,
        };
        p.draw(0);
        p
    }

    fn update(&mut self, done: u64) {
        let decile = match self.total {
            Some(total) => done * 10 / total.max(1),
            None => done / (4 * 1024 * 1024),
        };
        if decile != self.last_decile {
            self.last_decile = decile;
            self.draw(done);
        }
    }

    fn draw(&self, done: u64) {
        use std::io::Write;
        let mut err = std::io::stderr().lock();
        match self.total {
            Some(total) => {
                let total = total.max(1);
                let pct = (done * 100 / total).min(100);
                if self.tty {
                    let filled = (pct * 24 / 100) as usize;
                    let bar: String = "#".repeat(filled) + &"-".repeat(24 - filled);
                    let _ = write!(
                        err,
                        "\r{} [{bar}] {pct:>3}% ({:.1}/{:.1} MB)",
                        self.label,
                        mb(done),
                        mb(total)
                    );
                } else {
                    let _ = writeln!(err, "{} {pct}%", self.label);
                }
            }
            None => {
                if self.tty {
                    let _ = write!(err, "\r{} {:.1} MB", self.label, mb(done));
                } else {
                    let _ = writeln!(err, "{} {:.1} MB", self.label, mb(done));
                }
            }
        }
        let _ = err.flush();
    }

    fn finish(&self, done: u64) {
        use std::io::Write;
        self.draw(done);
        if self.tty {
            let _ = writeln!(std::io::stderr());
        }
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
