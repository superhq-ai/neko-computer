# Tunnel protocol

The protocol between the neko host connector and the edge. The edge is the `neko-edge` Worker plus a `Tunnel` Durable Object, one instance per subdomain.

The edge is a relay by design, not a shortcut: a web visitor is an arbitrary HTTPS client and cannot peer to a host behind NAT, so the public side must be a relay with a public address and TLS. Cloudflare provides the edge, wildcard TLS, DDoS protection, and hibernatable Durable Objects.

## Routing

The Worker reads the Host header. For `<sub>.neko.computer` it derives `<sub>` (the leftmost label) and forwards the request to the Durable Object `TUNNEL.idFromName(<sub>)`. The apex and `www` serve the landing page. Both the connector WebSocket and public traffic for a subdomain land on the same Durable Object.

## Connector registration

The connector opens a WebSocket:

```
GET wss://<sub>.neko.computer/__neko/connect
Authorization: Bearer <jwt>
```

The Worker verifies the JWT before routing the connect: a missing, invalid, or deny-listed token is refused here and the tunnel never opens. On success the Durable Object accepts the socket with the hibernation API and stores it as the connector for that subdomain. One connector per subdomain; a reconnect by the same account replaces the stored socket. WebSocket hibernation keeps an idle tunnel near zero cost.

## Name ownership

Ownership is implemented at the policy layer, not in the open transport: the Tunnel object consults the deployment policy before accepting a connector. A bare open-edge deployment applies no ownership at all. The semantics below are how the hosted neko.computer deployment behaves.

A subdomain can only carry one account at a time. A connect for a name whose live connector belongs to a different account is refused with 409, so a name can never be taken over mid-tunnel.

Generated names are held only while the connector is live; when the pipe breaks the name frees on its own, and nothing is stored for it.

Chosen names (the CLI adds `?claim=1` when the user passed `--subdomain`) are claimed by the first account to use them. The claim lives in the Tunnel Durable Object and blocks other accounts even between sessions, until it goes unused for 30 days, after which the name is up for grabs again. Claims are renewed on every connect and disconnect. The `Account` Durable Object caps how many names an account can hold (free 5, pro 50) and prunes expired claims before counting, so abandoned names stop counting against the cap. A rejected claim returns 429 with a distinct message.

## Framing

Binary WebSocket frames, `type(1) + id(4, big-endian) + payload`, multiplexed by request id over the one connector socket. Symmetric, so both directions stream over the same frames.

- `REQ_HEAD` (1): payload is JSON `{ method, url, headers }`. `url` is path plus query.
- `REQ_BODY` (2): payload is a raw body chunk.
- `REQ_END` (3): no payload.
- `RES_HEAD` (4): payload is JSON `{ status, headers }`.
- `RES_BODY` (5): payload is a raw body chunk.
- `RES_END` (6): no payload.
- `ABORT` (7): either side ends a request; no payload required.
- `WS_OPEN` (8): edge to connector, payload is JSON `{ url, headers }`. Opens a guest WebSocket for a visitor WebSocket.
- `WS_DATA` (9): either direction, payload is one kind byte (1 text, 0 binary) then the frame data.
- `WS_CLOSE` (10): either direction; the WebSocket for this id closed.

The edge sends `REQ_HEAD`, then streams the visitor request body as `REQ_BODY` frames as it reads them, then `REQ_END`, so nothing buffers the whole upload (capped at 100 MB by a running byte count). The edge drops `accept-encoding` from the forwarded headers so the origin answers identity: the Workers runtime treats response bodies as unencoded and re-encodes to match `content-encoding`, so relaying compressed bytes would double-compress them. Compression for the visitor happens at the edge on the way out. It waits for `RES_HEAD` to build the `Response`, then feeds each `RES_BODY` into a `ReadableStream` so the visitor gets bytes as they arrive, and closes on `RES_END`. The connector dispatches the guest request on `REQ_HEAD` with a streaming body, pushes each `REQ_BODY` into it, closes the body on `REQ_END`, then sends `RES_HEAD` and streams the guest response back as `RES_BODY` frames. The guest request uses chunked transfer-encoding since the length is not known up front.

## WebSocket passthrough

When a visitor request is itself a WebSocket upgrade, the Durable Object accepts the visitor socket (tagged by id, so hibernation can still route it) and sends `WS_OPEN` with the path and headers. The connector opens a WebSocket to the guest and relays: bytes each way become `WS_DATA` frames tagged with the id, a close on either end becomes `WS_CLOSE`. The visitor gets its `101` immediately, before the guest handshake exists, so the edge echoes the first subprotocol the visitor offered (browsers kill the socket when a requested subprotocol is not confirmed; Vite HMR offers `vite-hmr`). If the guest would have picked a different one of several offers, the visitor still sees the first; no known client cares. `Sec-WebSocket-Protocol` is forwarded to the guest and the handshake headers the relay manages (`Sec-WebSocket-Key`, `-Version`, `-Extensions`) are stripped so no compression is negotiated across the hops. One tunnel caps at 50 concurrent visitor WebSockets.

## Semantics

- No connector attached: 502. No `RES_HEAD` within 30s: 504.
- Concurrency cap: at most 20 in-flight requests per tunnel; over that returns 503.
- Idle timeout: a request with no `RES_BODY` for 120s is aborted. Max lifetime: 10 minutes.
- Memory guard: if the response buffer in the DO overflows (a slow visitor against a fast producer), the request is aborted rather than growing unbounded. This is the place a per-request credit window would slot in for true backpressure.
- Connector socket close aborts all in-flight requests; the next connect re-registers.

## Visitor auth (private tunnels)

A connector may open a tunnel whose visitors must prove membership of an
organization at the identity provider. Everything below is transport
mechanism; whether a given tunnel is private is a policy decision
(`ConnectVerdict` `allow` may carry `access: { org }`).

- Connect URL params: `private=1&org=<id>`. The worker forwards them to the
  Tunnel object as `p` and `o`; the policy sees `wantsPrivate` and `org` in
  its `ConnectContext` and decides.
- A request to a private tunnel without a valid access cookie is answered
  `302` to `TUNNEL_AUTH_URL` (default `${ISSUER}/api/tunnel-auth`) with
  query `host`, `org`, `redirect` (always `https://<host>/__neko/auth`),
  and `state` (the original path and query). WebSocket upgrades get `401`.
- The provider contract: authenticate the visitor however it likes, decide
  membership of `org`, then `302` back to `redirect` with `token` (and the
  untouched `state`). The token is an HS256 JWT signed with the shared
  `TUNNEL_AUTH_SECRET`: `sub` visitor id, `aud` the exact tunnel host,
  `org`, and an `exp` of its choosing. The provider must refuse `redirect`
  values that do not point at `/__neko/auth` on `host`.
- `/__neko/auth` verifies the token (signature, `aud` against Host, `org`
  against the value the tunnel registered) and plants it as the
  `__neko_access` cookie (HttpOnly, Secure, SameSite=Lax, path `/`), then
  redirects to `state`. Later requests verify the cookie locally.
- Private responses carry `x-robots-tag: noindex`.

## Guardrails

These are deployment-supplied through createEdge options and the policy seam; the open transport ships none of them on by default. As deployed on neko.computer, layered under the auth gate:

- **Reserved subdomains.** System names (`www`, `api`, `app`, `admin`, and so on) cannot be claimed as tunnels. `install` and `get` serve the install script instead.
- **Deny-list and kill switch.** The `DENYLIST` KV namespace disables tunnels: a key named for a subdomain blocks that one, a key named for a user id suspends that account on its next connect, and a `*` key blocks all. Operator commands:

  ```
  wrangler kv key put --binding DENYLIST <sub> 1        # block one tunnel
  wrangler kv key put --binding DENYLIST <user-id> 1    # suspend an account
  wrangler kv key put --binding DENYLIST '*' 1          # global kill switch
  ```

- **Rate limits.** The Tunnel Durable Object throttles requests with a token bucket (burst 100, refill 60 per second), returning 429 when exceeded, generous for a dev server but a cap against amplification. A `Limiter` Durable Object keyed by connector IP caps tunnel creation at 30 per 60 seconds, checked in the Worker before a connect is routed.
- **Per-account quotas.** The connector JWT carries the account plan. On connect the Worker enforces a daily creation cap with a `Limiter` keyed by user id, and the Tunnel Durable Object enforces a concurrent-tunnel cap through an `Account` Durable Object that holds the subdomains a user has open, keyed by name so a reconnect does not double count and a disconnect frees the slot. Free plan is 100 per day and 5 concurrent.

## Connector-served endpoints

With the web terminal enabled (`neko run --term`, sandbox backend, private
tunnels only), the connector intercepts `/__neko/term` before proxying.
When no app port is tunneled alongside it, every other path answers a 302
to the terminal. Behavior:
`GET /__neko/term` and paths under it serve the embedded terminal page, and
a WebSocket upgrade to `/__neko/term/ws?rows=R&cols=C` bridges to a fresh
PTY login shell in the sandbox. Over that socket, binary messages carry PTY
bytes in both directions; text messages carry JSON control (`{"type":
"resize", rows, cols}` from the page, `{"type": "exit", code}` from the
shell). The edge needs no changes: the paths relay like any visitor
traffic, and the private-tunnel gate (cookie for pages, 401 for ungated
WebSocket upgrades) applies before anything reaches the connector.

## Internal endpoints

The Tunnel object answers two endpoints meant for the deployment itself, not
for visitors, both gated on the `x-neko-probe` header. The worker strips that
header from public traffic before routing, so only the worker and sibling
objects can reach them; a visitor request for the same path proxies to the
origin normally.

- `GET /__neko/alive` answers 200 with `x-neko-user` when a connector is
  attached, 410 otherwise. Used to tell a live tunnel from a slot left behind
  by a connector that died without a clean close.
- `POST /__neko/kill?user=<id>` closes the connector (close code 1000, reason
  `closed from the console`) and every visitor socket, and frees the quota
  slot through the same release path a clean disconnect takes. The `user`
  param must match the account that opened the tunnel; 403 otherwise, 410
  when no connector is attached. How a kill is authorized end to end (console
  session, service secret) is a deployment concern outside the transport.

## Ingress

The connector reaches the sandbox port through the shuru SDK primitive `AsyncSandbox::dial_guest(port) -> GuestStream` (shuru-sdk 0.3.7), then serves guest responses back over the frames above.
