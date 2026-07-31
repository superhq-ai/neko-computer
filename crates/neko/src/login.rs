use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;

const CLIENT_ID: &str = "neko";

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval: Option<u64>,
}

pub async fn login() -> Result<()> {
    let auth_base = crate::dist::auth();
    let client = reqwest::Client::new();

    let dc: DeviceCode = client
        .post(format!("{auth_base}/api/auth/device/code"))
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let uri = dc
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&dc.verification_uri);
    println!("to sign in, open:\n  {uri}");
    println!("and confirm the code: {}", dc.user_code);

    let interval = Duration::from_secs(dc.interval.unwrap_or(5));
    loop {
        tokio::time::sleep(interval).await;

        let resp = client
            .post(format!("{auth_base}/api/auth/device/token"))
            .json(&serde_json::json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": dc.device_code,
                "client_id": CLIENT_ID,
            }))
            .send()
            .await?;

        let ok = resp.status().is_success();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();

        if ok {
            let token =
                extract_token(&body).ok_or_else(|| anyhow!("no token in token response"))?;
            store_token(&auth_base, &token)?;
            println!("signed in");
            return Ok(());
        }

        match body.get("error").and_then(|v| v.as_str()).unwrap_or("") {
            "authorization_pending" => continue,
            "slow_down" => tokio::time::sleep(Duration::from_secs(5)).await,
            other => return Err(anyhow!("device login failed: {other}")),
        }
    }
}

fn extract_token(body: &serde_json::Value) -> Option<String> {
    for key in ["access_token", "token", "session_token"] {
        if let Some(v) = body.get(key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    body.get("session")
        .and_then(|s| s.get("token"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn token_path() -> Result<std::path::PathBuf> {
    Ok(dirs::config_dir()
        .ok_or_else(|| anyhow!("no config directory"))?
        .join("neko")
        .join("token"))
}

// The session file records which auth base issued it, so a binary pointed at
// a different endpoint refuses the stale session instead of sending it there.
fn store_token(auth_base: &str, token: &str) -> Result<()> {
    let path = token_path()?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let record = serde_json::json!({ "auth": auth_base, "token": token });
    std::fs::write(&path, record.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_session_token(auth_base: &str) -> Result<String> {
    let raw = std::fs::read_to_string(token_path()?)
        .map_err(|_| anyhow!("not signed in, run: neko login"))?;
    // Sessions saved before the auth base was recorded are a bare token from
    // the built-in default endpoint.
    let (issued_by, token) = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) if v.get("token").is_some() => (
            v.get("auth")
                .and_then(|a| a.as_str())
                .unwrap_or(crate::dist::AUTH)
                .to_string(),
            v.get("token")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        _ => (crate::dist::AUTH.to_string(), raw.trim().to_string()),
    };
    if issued_by != auth_base {
        return Err(anyhow!(
            "stored session was issued by {issued_by}, not {auth_base}; run: neko login"
        ));
    }
    Ok(token)
}

pub struct Workspace {
    pub id: String,
    pub slug: String,
    pub personal: bool,
}

/// The workspaces the signed-in account belongs to, from the console API.
/// Used to resolve what --private may name; the edge verifies membership
/// again from the JWT, this lookup just fails fast with a good message.
pub async fn workspaces() -> Result<Vec<Workspace>> {
    let auth_base = crate::dist::auth();
    let session = load_session_token(&auth_base)?;
    let resp = reqwest::Client::new()
        .get(format!("{auth_base}/api/workspaces"))
        .bearer_auth(&session)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("session expired, run: neko login"));
    }
    let body: serde_json::Value = resp.json().await?;
    let list = body
        .get("workspaces")
        .and_then(|w| w.as_array())
        .ok_or_else(|| anyhow!("unexpected response from {auth_base}"))?;
    Ok(list
        .iter()
        .filter_map(|w| {
            Some(Workspace {
                id: w.get("id")?.as_str()?.to_string(),
                slug: w.get("slug")?.as_str()?.to_string(),
                personal: w.get("personal").and_then(|p| p.as_bool()).unwrap_or(false),
            })
        })
        .collect())
}

/// Resolve a --private selector: empty means the personal workspace, anything
/// else is a workspace slug the account must belong to. Returns (org id, label).
pub async fn resolve_workspace(selector: &str) -> Result<(String, String)> {
    let all = workspaces().await?;
    if selector.is_empty() {
        let personal = all
            .iter()
            .find(|w| w.personal)
            .ok_or_else(|| anyhow!("no personal workspace on this account"))?;
        return Ok((personal.id.clone(), "your personal workspace".to_string()));
    }
    match all.iter().find(|w| w.slug == selector) {
        Some(ws) => Ok((ws.id.clone(), format!("{} members", ws.slug))),
        None => Err(anyhow!(
            "you are not a member of a workspace with the slug {selector}"
        )),
    }
}

/// Ownership and visibility are separate. `--workspace` names the owner;
/// `--private [workspace]` keeps its existing shorthand, and omission resolves
/// deterministically to the personal SuperHQ workspace without a prompt.
pub async fn resolve_tunnel_workspace(
    workspace: Option<&str>,
    private: Option<&str>,
) -> Result<(String, String)> {
    resolve_workspace(&tunnel_workspace_selector(workspace, private)?).await
}

fn tunnel_workspace_selector(workspace: Option<&str>, private: Option<&str>) -> Result<String> {
    let private_workspace = private.filter(|selector| !selector.is_empty());
    if let (Some(owner), Some(audience)) = (workspace, private_workspace) {
        if owner != audience {
            bail!(
                "--workspace {owner} conflicts with --private {audience}; a private tunnel's owner and audience must match"
            );
        }
    }
    Ok(private_workspace.or(workspace).unwrap_or("").to_string())
}

/// Trade the stored SuperHQ session for a short-lived JWT the edge verifies
/// offline against the issuer JWKS. Fetched fresh per connect.
pub async fn access_jwt() -> Result<String> {
    let auth_base = crate::dist::auth();
    let session = load_session_token(&auth_base)?;
    let resp = reqwest::Client::new()
        .get(format!("{auth_base}/api/auth/token"))
        .bearer_auth(&session)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("session expired, run: neko login"));
    }
    let body: serde_json::Value = resp.json().await?;
    body.get("token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("no token in response"))
}

#[cfg(test)]
mod tests {
    use super::tunnel_workspace_selector;

    #[test]
    fn tunnel_workspace_selection_is_deterministic_and_compatible() {
        assert_eq!(tunnel_workspace_selector(None, None).unwrap(), "");
        assert_eq!(
            tunnel_workspace_selector(Some("acme"), None).unwrap(),
            "acme"
        );
        assert_eq!(tunnel_workspace_selector(None, Some("")).unwrap(), "");
        assert_eq!(
            tunnel_workspace_selector(None, Some("acme")).unwrap(),
            "acme"
        );
        assert_eq!(
            tunnel_workspace_selector(Some("acme"), Some("")).unwrap(),
            "acme"
        );
        assert_eq!(
            tunnel_workspace_selector(Some("acme"), Some("acme")).unwrap(),
            "acme"
        );
        assert!(tunnel_workspace_selector(Some("acme"), Some("other")).is_err());
    }
}
