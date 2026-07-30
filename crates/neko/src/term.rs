use include_dir::{include_dir, Dir};

/// The web terminal page, embedded at build time so the page and the PTY
/// bridge it speaks to always ship together in one binary.
static ASSETS: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/term");

pub const PREFIX: &str = "/__neko/term";
pub const WS_PATH: &str = "/__neko/term/ws";

/// Resolve a request path under PREFIX to (bytes, content type, cache).
pub fn asset(path: &str) -> Option<(&'static [u8], &'static str, &'static str)> {
    let rel = path.strip_prefix(PREFIX)?.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let file = ASSETS.get_file(rel)?;
    let kind = match rel.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    // Vendor files change only with a CLI release; the page shell stays fresh.
    let cache = if rel.starts_with("vendor/") {
        "public, max-age=86400"
    } else {
        "no-cache"
    };
    Some((file.contents(), kind, cache))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_serves_the_page() {
        let (_, kind, cache) = asset("/__neko/term").unwrap();
        assert_eq!(kind, "text/html; charset=utf-8");
        assert_eq!(cache, "no-cache");
        assert!(asset("/__neko/term/").is_some());
    }

    #[test]
    fn vendor_files_resolve_and_cache() {
        let (_, kind, cache) = asset("/__neko/term/vendor/core/index.js").unwrap();
        assert_eq!(kind, "text/javascript; charset=utf-8");
        assert_eq!(cache, "public, max-age=86400");
    }

    #[test]
    fn unknown_and_escaping_paths_are_refused() {
        assert!(asset("/__neko/term/nope.js").is_none());
        assert!(asset("/__neko/term/../../Cargo.toml").is_none());
        assert!(asset("/somewhere/else").is_none());
    }
}
