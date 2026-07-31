import { jwtVerify } from "jose";
import { activeLimits, activePolicy, contain, containAsync, report } from "./policy";

const CONNECT_PATH = "/__neko/connect";
// Liveness probe, used by a deployment to tell a live tunnel from a slot left
// behind by a connector that died without a clean close. Gated on a header so
// a visitor path of the same name still proxies to the origin normally.
const ALIVE_PATH = "/__neko/alive";
// Owner-initiated kill: drops the connector and every visitor socket, freeing
// the quota slot through the same release path a clean disconnect takes.
const KILL_PATH = "/__neko/kill";
// Gates the internal endpoints above. The worker strips this header from
// public traffic before routing, so only the deployment itself can speak it;
// a visitor path of the same name still proxies to the origin normally.
export const PROBE_HEADER = "x-neko-probe";
// Private tunnel visitor auth: the identity provider redirects back here
// with a token, which becomes the access cookie for this subdomain.
const AUTH_PATH = "/__neko/auth";
const ACCESS_COOKIE = "__neko_access";
const ACCESS_MAX_AGE = 43_200;

// Frame types: type(1) + id(4, big-endian) + payload. Symmetric so both
// directions stream over the same frames.
const REQ_HEAD = 1;
const REQ_BODY = 2;
const REQ_END = 3;
const RES_HEAD = 4;
const RES_BODY = 5;
const RES_END = 6;
const ABORT = 7;
const WS_OPEN = 8;
// WS_DATA payload is one kind byte (1 text, 0 binary) then the frame data.
const WS_DATA = 9;
const WS_CLOSE = 10;

// Statuses the Fetch spec forbids a body on. A browser reload sends validators
// and origins answer 304, so attaching the response stream to one of these
// throws a TypeError.
const NULL_BODY_STATUS = new Set([101, 204, 205, 304]);

export function forbidsBody(status: number): boolean {
  return NULL_BODY_STATUS.has(status);
}

interface Attachment {
  user: string;
  sub: string;
  // Resource owner, separate from private visitor access. Missing only on a
  // connector accepted before workspace ownership shipped.
  owner?: { org: string };
  gate: boolean;
  redirect?: string;
  // Present on a private tunnel: visitors need a tunnel-auth token for org.
  access?: { org: string };
}

interface Pending {
  controller: ReadableStreamDefaultController<Uint8Array>;
  resolveHead: (meta: { status: number; headers: Record<string, string> }) => void;
  rejectHead: (reason: unknown) => void;
  headSettled: boolean;
  idle: ReturnType<typeof setTimeout>;
  lifetime: ReturnType<typeof setTimeout>;
}

export class Tunnel {
  private ctx: DurableObjectState;
  private env: unknown;
  private connector: WebSocket | null;
  private pending = new Map<number, Pending>();
  private nextId = 1;
  private tokens: number;
  private lastRefill = 0;

  constructor(ctx: DurableObjectState, env: unknown) {
    this.ctx = ctx;
    this.env = env;
    this.connector = ctx.getWebSockets("connector")[0] ?? null;
    this.tokens = activeLimits().bucket?.burst ?? 0;
  }

  async fetch(request: Request): Promise<Response> {
    try {
      return await this.serve(request);
    } catch (e) {
      report("tunnel.fetch", e, this.env);
      return new Response("the tunnel hit an internal error\n", { status: 500 });
    }
  }

  private async serve(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const limits = activeLimits();

    if (url.pathname === ALIVE_PATH && request.headers.get(PROBE_HEADER)) {
      const att = this.connector?.deserializeAttachment() as Attachment | null;
      return new Response(null, {
        status: this.connector ? 200 : 410,
        headers: att?.user
          ? {
              "x-neko-user": att.user,
              ...(att.owner?.org ? { "x-neko-org": att.owner.org } : {}),
            }
          : undefined,
      });
    }

    if (url.pathname === KILL_PATH && request.headers.get(PROBE_HEADER)) {
      if (request.method !== "POST") {
        return new Response(null, { status: 405 });
      }
      const ws = this.connector;
      if (!ws) {
        return new Response(null, { status: 410 });
      }
      const att = ws.deserializeAttachment() as Attachment | null;
      const user = url.searchParams.get("user");
      const org = url.searchParams.get("org");
      if (!att?.user) {
        return new Response(null, { status: 403 });
      }
      if (org && !att.owner?.org) return new Response(null, { status: 428 });
      if (org ? att.owner?.org !== org : !user || att.user !== user) {
        return new Response(null, { status: 403 });
      }
      try {
        ws.close(1000, "closed from the console");
      } catch {
        /* already closed */
      }
      // release() frees the quota slot and drops visitor sockets; a late
      // webSocketClose for this socket is ignored once the connector is gone.
      await this.release(ws);
      return new Response(null, { status: 200 });
    }

    if (url.pathname === CONNECT_PATH) {
      if (request.headers.get("Upgrade") !== "websocket") {
        return new Response("expected websocket upgrade\n", { status: 426 });
      }
      const user = url.searchParams.get("u") ?? "";
      const sub = url.searchParams.get("s") ?? "";
      const wantsClaim = url.searchParams.get("c") === "1";
      const wantsPrivate = url.searchParams.get("p") === "1";
      const org = url.searchParams.get("o") || null;
      const ip = url.searchParams.get("i") ?? "";
      let claims: Record<string, unknown> = {};
      try {
        claims = JSON.parse(url.searchParams.get("y") ?? "{}");
      } catch {
        /* leave empty */
      }

      const verdict = await activePolicy().connect(
        {
          userId: user,
          sub,
          wantsClaim,
          wantsPrivate,
          org,
          liveUser: this.liveUser(),
          liveOrg: this.liveOrg(),
          claims,
          ip,
        },
        this.env,
      );
      if (verdict.action === "deny") {
        return new Response(`${verdict.message}\n`, { status: verdict.status });
      }

      const prev = this.connector;
      const [client, server] = Object.values(new WebSocketPair());
      this.ctx.acceptWebSocket(server, ["connector"]);
      const att: Attachment = {
        user,
        sub,
        owner: verdict.owner,
        gate: verdict.action === "gate",
        redirect: verdict.action === "gate" ? verdict.redirect : undefined,
        access: verdict.action === "allow" ? verdict.access : undefined,
      };
      server.serializeAttachment(att);
      this.connector = server;
      if (prev) {
        try {
          prev.close(1012, "replaced by a new connector");
        } catch {
          /* already closed */
        }
      }
      return new Response(null, { status: 101, webSocket: client });
    }

    if (!this.connector) {
      return new Response("no connector attached to this tunnel\n", { status: 502 });
    }
    const att = this.connector.deserializeAttachment() as Attachment | null;
    if (att?.gate) {
      if (request.headers.get("Upgrade") === "websocket") {
        return new Response("this tunnel is gated\n", { status: 402 });
      }
      return new Response(null, {
        status: 302,
        headers: { location: att.redirect ?? "/" },
      });
    }
    if (att?.access) {
      const wall = await this.admitVisitor(request, url, att.access.org);
      if (wall) {
        return wall;
      }
    }
    if (!this.allow(limits)) {
      return new Response("rate limit exceeded\n", { status: 429 });
    }

    if (request.headers.get("Upgrade") === "websocket") {
      return this.openWebSocket(url, request, limits);
    }

    if (limits.maxConcurrent !== undefined && this.pending.size >= limits.maxConcurrent) {
      return new Response("too many concurrent streams on this tunnel\n", { status: 503 });
    }

    const id = this.nextId++;
    const self = this;
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const stream = new ReadableStream<Uint8Array>(
      {
        start(c) {
          controller = c;
        },
        cancel() {
          self.abort(id, "client cancelled");
        },
      },
      new ByteLengthQueuingStrategy({ highWaterMark: limits.highWater }),
    );

    const head = new Promise<{ status: number; headers: Record<string, string> }>((resolve, reject) => {
      this.pending.set(id, {
        controller,
        resolveHead: resolve,
        rejectHead: reject,
        headSettled: false,
        idle: setTimeout(() => this.abort(id, "idle timeout"), limits.idleTimeoutMs),
        lifetime: setTimeout(() => this.abort(id, "max lifetime"), limits.maxLifetimeMs),
      });
    });
    // Streaming an upload yields, and an unreachable origin can abort during
    // that window, before the race below attaches a rejection handler. An
    // unhandled rejection takes down the whole isolate, so claim it now; the
    // race still sees the original outcome.
    head.catch(() => {});

    try {
      this.connector.send(
        this.frame(
          REQ_HEAD,
          id,
          utf8(
            JSON.stringify({
              method: request.method,
              url: url.pathname + url.search,
              headers: relayHeaders(request.headers),
            }),
          ),
        ),
      );
      if (request.body) {
        const reader = request.body.getReader();
        let total = 0;
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          total += value.byteLength;
          if (total > limits.requestBodyCap) {
            this.connector.send(this.frame(ABORT, id));
            this.cleanup(id);
            return new Response("request body too large\n", { status: 413 });
          }
          this.connector.send(this.frame(REQ_BODY, id, value));
        }
      }
      this.connector.send(this.frame(REQ_END, id));
    } catch {
      this.cleanup(id);
      return new Response("connector unavailable\n", { status: 502 });
    }

    let timer!: ReturnType<typeof setTimeout>;
    const headTimeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error("head timeout")), limits.headTimeoutMs);
    });
    headTimeout.catch(() => {});
    let meta: { status: number; headers: Record<string, string> };
    try {
      meta = await Promise.race([head, headTimeout]);
    } catch (e) {
      const why = e instanceof Error ? e.message : "";
      this.abort(id, "upstream timeout");
      // A connector that named the failure gets a 502 with that reason; a
      // silent origin is a plain timeout.
      return why && why !== "head timeout"
        ? new Response(`the tunnel could not reach your server: ${why}\n`, { status: 502 })
        : new Response("tunnel timeout\n", { status: 504 });
    } finally {
      clearTimeout(timer);
    }
    if (att?.access) {
      // Private content stays out of indexes even if a token holder shares
      // a page with a crawler-adjacent tool.
      meta.headers = { ...meta.headers, "x-robots-tag": "noindex" };
    }
    if (forbidsBody(meta.status)) {
      // No body is coming; let the stream be collected when RES_END lands.
      return new Response(null, { status: meta.status, headers: meta.headers });
    }
    return new Response(stream, { status: meta.status, headers: meta.headers });
  }

  // The visitor side of a private tunnel: a valid access cookie admits, the
  // auth callback plants it, anything else bounces to the identity provider
  // with the original path carried in state. Null means admitted.
  private async admitVisitor(request: Request, url: URL, org: string): Promise<Response | null> {
    const env = this.env as
      | { TUNNEL_AUTH_SECRET?: string; TUNNEL_AUTH_URL?: string; ISSUER?: string }
      | undefined;
    const secret = env?.TUNNEL_AUTH_SECRET;
    const authUrl = env?.TUNNEL_AUTH_URL ?? (env?.ISSUER ? `${env.ISSUER}/api/tunnel-auth` : "");
    const host = (request.headers.get("host") ?? url.hostname).toLowerCase().split(":")[0] ?? "";
    if (!secret || !authUrl) {
      return new Response("private tunnels are not configured on this edge\n", { status: 503 });
    }

    if (url.pathname === AUTH_PATH) {
      const token = url.searchParams.get("token") ?? "";
      if (!(await this.visitorToken(token, secret, host, org))) {
        return new Response("tunnel sign in failed; open the tunnel URL again\n", { status: 403 });
      }
      const state = url.searchParams.get("state") ?? "/";
      const back = state.startsWith("/") && !state.startsWith("//") ? state : "/";
      return new Response(null, {
        status: 302,
        headers: {
          location: back,
          "set-cookie": `${ACCESS_COOKIE}=${token}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=${ACCESS_MAX_AGE}`,
          "x-robots-tag": "noindex",
        },
      });
    }

    const cookie = readCookie(request.headers.get("cookie") ?? "", ACCESS_COOKIE);
    if (cookie && (await this.visitorToken(cookie, secret, host, org))) {
      return null;
    }
    if (request.headers.get("Upgrade") === "websocket") {
      return new Response("this tunnel is private\n", { status: 401 });
    }
    const back = new URL(`https://${host}${AUTH_PATH}`);
    const auth = new URL(authUrl);
    auth.searchParams.set("host", host);
    auth.searchParams.set("org", org);
    auth.searchParams.set("redirect", back.toString());
    auth.searchParams.set("state", url.pathname + url.search);
    return new Response(null, {
      status: 302,
      headers: { location: auth.toString(), "x-robots-tag": "noindex" },
    });
  }

  private async visitorToken(token: string, secret: string, host: string, org: string): Promise<boolean> {
    if (!token) {
      return false;
    }
    try {
      const { payload } = await jwtVerify(token, utf8(secret), { audience: host });
      return payload.org === org;
    } catch {
      return false;
    }
  }

  private openWebSocket(url: URL, request: Request, limits: ReturnType<typeof activeLimits>): Response {
    if (limits.maxWs !== undefined && this.ctx.getWebSockets().length - 1 >= limits.maxWs) {
      return new Response("too many websockets on this tunnel\n", { status: 503 });
    }
    const id = this.nextId++;
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server, [`v:${id}`]);
    try {
      this.connector!.send(
        this.frame(
          WS_OPEN,
          id,
          utf8(JSON.stringify({ url: url.pathname + url.search, headers: relayHeaders(request.headers) })),
        ),
      );
    } catch {
      try {
        server.close(1011, "connector unavailable");
      } catch {
        /* already closed */
      }
      return new Response("connector unavailable\n", { status: 502 });
    }
    // Browsers kill the socket when a requested subprotocol is not confirmed
    // in the 101 (Vite HMR offers vite-hmr). We accept before the guest
    // handshake exists, so echo the first offered protocol.
    const offered = request.headers.get("sec-websocket-protocol");
    const headers = offered
      ? { "sec-websocket-protocol": (offered.split(",")[0] ?? "").trim() }
      : undefined;
    return new Response(null, { status: 101, webSocket: client, headers });
  }

  // Every entry point below is contained: a malformed frame or an unexpected
  // state must degrade one request, never tear down the actor and with it
  // every live tunnel on this object.
  webSocketMessage(ws: WebSocket, message: string | ArrayBuffer) {
    contain("tunnel.webSocketMessage", () => this.onMessage(ws, message), this.env);
  }

  private onMessage(ws: WebSocket, message: string | ArrayBuffer) {
    if (this.ctx.getTags(ws).includes("connector")) {
      this.onControl(message);
      return;
    }
    const id = visitorId(this.ctx.getTags(ws));
    if (id === null || !this.connector) return;
    const data = typeof message === "string" ? utf8(message) : new Uint8Array(message);
    const payload = new Uint8Array(1 + data.length);
    payload[0] = typeof message === "string" ? 1 : 0;
    payload.set(data, 1);
    try {
      this.connector.send(this.frame(WS_DATA, id, payload));
    } catch {
      /* connector gone */
    }
  }

  private onControl(message: string | ArrayBuffer) {
    if (typeof message === "string") return;
    if (message.byteLength < 5) return;
    const view = new DataView(message);
    const type = view.getUint8(0);
    const id = view.getUint32(1, false);
    const payload = new Uint8Array(message, 5);

    if (type === WS_DATA) {
      const target = this.ctx.getWebSockets(`v:${id}`)[0];
      if (target) {
        const data = payload.subarray(1);
        try {
          payload[0] === 1 ? target.send(fromUtf8(data)) : target.send(data);
        } catch {
          /* visitor gone */
        }
      }
      return;
    }
    if (type === WS_CLOSE) {
      const target = this.ctx.getWebSockets(`v:${id}`)[0];
      try {
        target?.close(1000);
      } catch {
        /* already closed */
      }
      return;
    }

    const p = this.pending.get(id);
    if (!p) return;

    switch (type) {
      case RES_HEAD: {
        try {
          const meta = JSON.parse(fromUtf8(payload));
          p.headSettled = true;
          p.resolveHead({ status: meta.status ?? 502, headers: meta.headers ?? {} });
        } catch {
          this.abort(id, "bad response head");
          return;
        }
        this.touch(p, id);
        break;
      }
      case RES_BODY: {
        try {
          p.controller.enqueue(payload);
        } catch {
          this.cleanup(id);
          return;
        }
        if ((p.controller.desiredSize ?? 0) < -activeLimits().responseOverflow) {
          this.abort(id, "response buffer overflow");
          return;
        }
        this.touch(p, id);
        break;
      }
      case RES_END: {
        try {
          p.controller.close();
        } catch {
          /* already closed */
        }
        this.cleanup(id);
        break;
      }
      case ABORT: {
        // The connector explains why it could not serve the request (for
        // example nothing listening on the port); pass it on instead of
        // letting it surface as a bare timeout.
        const reason = payload.length > 0 ? fromUtf8(payload) : "aborted by connector";
        this.failStream(p, reason);
        this.cleanup(id);
        break;
      }
    }
  }

  async webSocketClose(ws: WebSocket) {
    await containAsync("tunnel.webSocketClose", () => this.release(ws), this.env);
  }
  async webSocketError(ws: WebSocket) {
    await containAsync("tunnel.webSocketError", () => this.release(ws), this.env);
  }

  private async release(ws: WebSocket) {
    const tags = this.ctx.getTags(ws);
    if (!tags.includes("connector")) {
      const id = visitorId(tags);
      if (id !== null) {
        try {
          this.connector?.send(this.frame(WS_CLOSE, id));
        } catch {
          /* connector gone */
        }
      }
      return;
    }
    if (ws !== this.connector) {
      // A replaced socket closing late; the live connector still owns the tunnel.
      return;
    }
    const att = ws.deserializeAttachment() as Attachment | null;
    if (att?.user && att?.sub) {
      await activePolicy().closed?.(
        { userId: att.user, org: att.owner?.org ?? null, sub: att.sub },
        this.env,
      );
    }
    this.dropConnector();
  }

  // The transport stores nothing; any storage or alarm found here is left over
  // from an earlier deployment and is wiped so the object can hibernate clean.
  async alarm() {
    await containAsync("tunnel.alarm", () => this.ctx.storage.deleteAll(), this.env);
  }

  private liveUser(): string | null {
    if (!this.connector) return null;
    const att = this.connector.deserializeAttachment() as Attachment | null;
    return att?.user ?? "";
  }

  private liveOrg(): string | null {
    if (!this.connector) return null;
    const att = this.connector.deserializeAttachment() as Attachment | null;
    return att?.owner?.org ?? null;
  }

  private dropConnector() {
    this.connector = null;
    for (const [id, p] of this.pending) {
      this.failStream(p, "connector disconnected");
      clearTimeout(p.idle);
      clearTimeout(p.lifetime);
      this.pending.delete(id);
    }
    for (const ws of this.ctx.getWebSockets()) {
      try {
        ws.close(1011, "connector gone");
      } catch {
        /* already closed */
      }
    }
  }

  private touch(p: Pending, id: number) {
    clearTimeout(p.idle);
    p.idle = setTimeout(() => this.abort(id, "idle timeout"), activeLimits().idleTimeoutMs);
  }

  private abort(id: number, reason: string) {
    const p = this.pending.get(id);
    if (!p) return;
    try {
      this.connector?.send(this.frame(ABORT, id));
    } catch {
      /* connector gone */
    }
    this.failStream(p, reason);
    this.cleanup(id);
  }

  private failStream(p: Pending, reason: string) {
    if (p.headSettled) {
      try {
        p.controller.error(new Error(reason));
      } catch {
        /* already closed */
      }
    } else {
      p.rejectHead(new Error(reason));
    }
  }

  private cleanup(id: number) {
    const p = this.pending.get(id);
    if (!p) return;
    clearTimeout(p.idle);
    clearTimeout(p.lifetime);
    this.pending.delete(id);
  }

  private frame(type: number, id: number, payload?: Uint8Array): ArrayBuffer {
    const buf = new Uint8Array(5 + (payload ? payload.length : 0));
    buf[0] = type;
    new DataView(buf.buffer).setUint32(1, id, false);
    if (payload) buf.set(payload, 5);
    return buf.buffer;
  }

  private allow(limits: ReturnType<typeof activeLimits>): boolean {
    const bucket = limits.bucket;
    if (!bucket) return true;
    const now = Date.now();
    if (this.lastRefill === 0) this.lastRefill = now;
    this.tokens = Math.min(bucket.burst, this.tokens + ((now - this.lastRefill) / 1000) * bucket.perSec);
    this.lastRefill = now;
    if (this.tokens >= 1) {
      this.tokens -= 1;
      return true;
    }
    return false;
  }
}

// Headers forwarded to the origin. accept-encoding is dropped so the origin
// answers identity: the Workers runtime treats response bodies as unencoded
// and re-encodes to match content-encoding, so relaying compressed bytes
// double-compresses them. The edge compresses for the visitor on the way out.
function relayHeaders(headers: Headers): Record<string, string> {
  const out = Object.fromEntries(headers);
  delete out["accept-encoding"];
  return out;
}

function readCookie(header: string, name: string): string {
  for (const part of header.split(";")) {
    const [key, ...rest] = part.trim().split("=");
    if (key === name) {
      return rest.join("=");
    }
  }
  return "";
}

function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}
function fromUtf8(b: Uint8Array): string {
  return new TextDecoder().decode(b);
}
function visitorId(tags: string[]): number | null {
  const tag = tags.find((t) => t.startsWith("v:"));
  return tag ? Number(tag.slice(2)) : null;
}
