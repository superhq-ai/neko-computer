//! Host directories offered to the sandbox. The current directory is mounted
//! at `/workspace` read-only unless asked otherwise: a sandbox that can
//! rewrite the tree it was launched from is not much of a sandbox.
//!
//! Read-only is not a wall the command hits. The guest lays an overlay over
//! the share, so writes succeed and land in a scratch layer that goes away
//! with the sandbox: `npm install` works, the host tree is untouched. Only
//! `--write` (a plain read-write share) puts the writes on the host.

use anyhow::{anyhow, bail, Result};
use shuru_sdk::MountConfig;

/// Where the current directory lands when no `--mount` is given.
pub const WORKSPACE: &str = "/workspace";

/// The default mount: the current directory at [`WORKSPACE`].
pub fn current_dir(read_only: bool) -> Result<MountConfig> {
    Ok(MountConfig {
        host_path: std::env::current_dir()?.to_string_lossy().into_owned(),
        guest_path: WORKSPACE.to_string(),
        read_only,
    })
}

/// `.:/src` mounts the current directory read-only at `/src`. A trailing
/// `:rw` allows writes; `:ro` is the default and may be spelled out. The host
/// path is resolved here so a typo costs an error rather than a boot.
pub fn parse(spec: &str) -> Result<MountConfig> {
    let (paths, read_only) = match spec.rsplit_once(':') {
        Some((rest, "rw")) => (rest, false),
        Some((rest, "ro")) => (rest, true),
        _ => (spec, true),
    };
    let (host, guest) = paths
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("mount needs HOST:GUEST, as in .:/workspace: {spec}"))?;
    let (host, guest) = (host.trim(), guest.trim());
    if host.is_empty() {
        bail!("mount has no host path: {spec}");
    }
    if !guest.starts_with('/') {
        bail!("mount guest path must be absolute: {spec}");
    }
    let guest = guest.trim_end_matches('/');
    if guest.is_empty() {
        bail!("mount guest path must not be /: {spec}");
    }
    let host_path = std::fs::canonicalize(host).map_err(|e| anyhow!("cannot mount {host}: {e}"))?;
    if !host_path.is_dir() {
        bail!("not a directory: {host}");
    }
    Ok(MountConfig {
        host_path: host_path.to_string_lossy().into_owned(),
        guest_path: guest.to_string(),
        read_only,
    })
}
