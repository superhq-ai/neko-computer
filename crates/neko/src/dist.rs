//! Distribution defaults and their runtime overrides. Forks rebrand by
//! editing the constants; users of an official binary point elsewhere with
//! env vars or the config file, no rebuild needed. Resolution order:
//! env var, then config.json next to the stored token, then the constant.

use std::sync::OnceLock;

pub const DOMAIN: &str = "neko.computer";
pub const AUTH: &str = "https://app.superhq.ai";
/// GitHub repo this binary self-upgrades from. Compile-time only: an upgrade
/// source must match the provenance of the running binary, not a runtime whim.
pub const REPO: &str = "superhq-ai/neko-computer";
/// GitHub repo the base sandbox image downloads from, and the release pin.
/// The pin is compile-time only: paths.rs re-provisions when it moves, so a
/// runtime override would desync the image from what the code expects.
pub const SHURU_REPO: &str = "superhq-ai/shuru";
pub const SHURU_OS_VERSION: &str = "0.6.4";

pub fn domain() -> String {
    resolve("NEKO_DOMAIN", "domain", DOMAIN)
}

/// A GitHub token for release lookups and downloads, used only when set.
/// Unauthenticated GitHub allows 60 API calls an hour per address, which a
/// shared office address or CI can exhaust.
///
/// Deliberately NOT reading GITHUB_TOKEN or GH_TOKEN: those are ambient in CI
/// and in many shells, so honouring them would let an expired or wrong-scope
/// token turn a working upgrade into a 401, which is worse than the anonymous
/// request it replaced. Opting in is explicit.
pub const GITHUB_TOKEN_VAR: &str = "NEKO_GITHUB_TOKEN";

pub fn github_token() -> Option<String> {
    std::env::var(GITHUB_TOKEN_VAR)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn auth() -> String {
    resolve("NEKO_AUTH_URL", "auth", AUTH)
        .trim_end_matches('/')
        .to_string()
}

fn resolve(env_key: &str, config_key: &str, default: &str) -> String {
    if let Ok(v) = std::env::var(env_key) {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    if let Some(v) = config_value(config_key) {
        return v;
    }
    default.to_string()
}

fn config_value(key: &str) -> Option<String> {
    static CONFIG: OnceLock<Option<serde_json::Value>> = OnceLock::new();
    CONFIG
        .get_or_init(load_config)
        .as_ref()?
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn load_config() -> Option<serde_json::Value> {
    let path = dirs::config_dir()?.join("neko").join("config.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
