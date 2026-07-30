# Self-hosting

## The edge

`edge/` is a library, not a deployment. `createEdge()` builds a Worker that owns identity verification (JWT against your issuer's JWKS) and the tunnel transport; everything that decides what an identity may do (name ownership, quotas, deny-lists, gating, pages) is supplied through its options. A bare `createEdge()` deployment is deliberately permissive: any authenticated user may open any name. See `edge/src/policy.ts` for the interface and [specs/tunnel-protocol.md](specs/tunnel-protocol.md) for the wire protocol.

A deployment needs its own Cloudflare KV namespace for the deny-list and the `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` deploy secrets.

## Pointing the CLI at another edge

The CLI defaults to `neko.computer` and `app.superhq.ai`. To use it against a self-hosted edge without rebuilding, set env vars or drop a config file; resolution is env, then file, then built-in default:

```sh
NEKO_DOMAIN=tunnels.example.com NEKO_AUTH_URL=https://auth.example.com neko tunnel 3000

# or persistently, in the config dir next to the stored token:
#   ~/Library/Application Support/neko/config.json (macOS)
#   ~/.config/neko/config.json (Linux)
{ "domain": "tunnels.example.com", "auth": "https://auth.example.com" }
```

Sessions are bound to the auth base that issued them; switching endpoints asks for a fresh `neko login` instead of sending the old session anywhere else. Forks rebrand the built-in defaults (domain, auth, upgrade repo, base image source) in one place: `crates/neko/src/dist.rs`.
