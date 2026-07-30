//! GitHub requests. Every call goes through here so an explicitly configured
//! token applies everywhere: release lookups, the CLI tarball, and the base
//! image. Anonymous by default; see dist::GITHUB_TOKEN_VAR for why opting in
//! is explicit rather than picked up from the ambient environment.

use reqwest::{RequestBuilder, Response};

use crate::dist;

fn authenticated(builder: RequestBuilder) -> RequestBuilder {
    let builder = builder.header("user-agent", "neko-cli");
    match dist::github_token() {
        Some(token) => builder.bearer_auth(token),
        None => builder,
    }
}

/// A GET carrying the token when one is configured.
pub fn get(url: &str) -> RequestBuilder {
    authenticated(reqwest::Client::new().get(url))
}

/// Turn a rate-limit refusal into advice instead of a bare 403.
pub fn explain(response: Response) -> reqwest::Result<Response> {
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "0")
        .unwrap_or(false);
    let refused = response.status() == 403 || response.status() == 429;
    if refused && remaining {
        match dist::github_token() {
            None => eprintln!(
                "github is rate limiting this address; retry with {}=<personal access token>",
                dist::GITHUB_TOKEN_VAR
            ),
            Some(_) => eprintln!(
                "github refused the request even with {}; check the token is valid and unexpired",
                dist::GITHUB_TOKEN_VAR
            ),
        }
    }
    response.error_for_status()
}
