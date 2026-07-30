import { createRemoteJWKSet, jwtVerify } from "jose";
import type { Env } from "./env";
import { configure, type Policy, type TransportLimits } from "./policy";
import { PROBE_HEADER } from "./tunnel";

export { Limiter } from "./limiter";
export { allowAll, DEFAULT_LIMITS } from "./policy";
export type { ConnectContext, ConnectVerdict, Policy, TransportLimits } from "./policy";
export { PROBE_HEADER, Tunnel } from "./tunnel";
export type { Env } from "./env";

const CONNECT_PATH = "/__neko/connect";
const INSTALL_SCRIPT = "https://raw.githubusercontent.com/superhq-ai/neko-computer/main/install.sh";

export interface EdgeOptions<E extends Env = Env> {
  policy?: Policy<E>;
  limits?: Partial<TransportLimits>;
  // Subdomains that must never resolve to a tunnel.
  reserved?: (sub: string) => boolean;
  // Per-request block check for a subdomain (deny-lists live here).
  blocked?: (sub: string, env: E) => Promise<boolean>;
  pages?: {
    landing?: (env: E) => Response;
    favicon?: () => Response;
  };
  // Called when a request throws. Cloudflare's own error page hides the stack
  // from anyone without log access, so a deployment can persist it here.
  onError?: (report: ErrorReport, env: E) => void | Promise<void>;
}

export interface ErrorReport {
  message: string;
  stack?: string;
  url: string;
  method: string;
  ray: string;
  at: number;
}

// Build the edge worker. The returned handler owns identity verification and
// routing; the Tunnel Durable Object owns the transport; everything else is
// supplied through options and the policy seam.
export function createEdge<E extends Env = Env>(options: EdgeOptions<E> = {}): ExportedHandler<E> {
  // Last resort. An unhandled rejection tears down the isolate and with it
  // every tunnel it was serving, which is far worse than the request that
  // caused it failing alone. Claim it, record it, keep serving.
  let lastEnv: E | undefined;
  const reporter = (where: string, error: unknown, env?: unknown) => {
    const report: ErrorReport = {
      message: error instanceof Error ? error.message : String(error),
      stack: error instanceof Error ? error.stack : undefined,
      url: where,
      method: "internal",
      ray: "",
      at: Date.now(),
    };
    console.error("edge error", JSON.stringify(report));
    // Durable Object handlers pass their own env, since they never run the
    // worker fetch that records one.
    const target = (env as E | undefined) ?? lastEnv;
    if (options.onError && target) {
      void Promise.resolve(options.onError(report, target)).catch(() => {});
    }
  };
  try {
    (globalThis as unknown as EventTarget).addEventListener?.("unhandledrejection", (event) => {
      const e = event as unknown as { reason?: unknown; preventDefault?: () => void };
      reporter("unhandledrejection", e.reason);
      e.preventDefault?.();
    });
  } catch {
    /* runtime without the event; the contained handlers still apply */
  }

  configure((options.policy ?? passthrough()) as Policy, options.limits, reporter);
  const reserved = options.reserved ?? (() => false);
  const blocked = options.blocked ?? (async () => false);
  const landing = options.pages?.landing ?? defaultLanding;
  const favicon = options.pages?.favicon ?? (() => new Response(null, { status: 404 }));

  return {
    async fetch(request: Request, env: E, ctx: ExecutionContext): Promise<Response> {
      lastEnv = env;
      try {
        return await route(request, env);
      } catch (e) {
        const report: ErrorReport = {
          message: e instanceof Error ? e.message : String(e),
          stack: e instanceof Error ? e.stack : undefined,
          url: request.url,
          method: request.method,
          ray: request.headers.get("cf-ray") ?? "",
          at: Date.now(),
        };
        console.error("edge error", JSON.stringify(report));
        if (options.onError) {
          ctx.waitUntil(Promise.resolve(options.onError(report, env)).catch(() => {}));
        }
        return text("the tunnel edge hit an internal error\n", 500);
      }
    },
  };

  async function route(request: Request, env: E): Promise<Response> {
      // The probe header authorizes internal Tunnel endpoints (liveness,
      // kill). Only this deployment may speak it, so a copy arriving from
      // the public internet is dropped before routing.
      if (request.headers.has(PROBE_HEADER)) {
        request = new Request(request);
        request.headers.delete(PROBE_HEADER);
      }
      const url = new URL(request.url);
      const host = (request.headers.get("host") ?? url.hostname).toLowerCase();
      const sub = subdomainOf(host, domain(env));

      if (!sub || sub === "www") {
        if (url.pathname === "/favicon.svg" || url.pathname === "/favicon.ico") {
          return favicon();
        }
        return landing(env);
      }

      if (sub === "install" || sub === "get") {
        return serveInstaller();
      }

      if (reserved(sub)) {
        return text("this name is reserved\n", 404);
      }

      if (await blocked(sub, env)) {
        return text("this tunnel has been disabled\n", 403);
      }

      if (url.pathname === CONNECT_PATH) {
        const auth = await authenticate(request, env);
        if ("error" in auth) {
          return auth.error;
        }
        url.searchParams.set("u", auth.userId);
        url.searchParams.set("s", sub);
        url.searchParams.set("c", url.searchParams.get("claim") === "1" ? "1" : "0");
        url.searchParams.set("p", url.searchParams.get("private") === "1" ? "1" : "0");
        url.searchParams.set("o", url.searchParams.get("org") ?? "");
        url.searchParams.set("y", JSON.stringify(auth.payload));
        url.searchParams.set("i", request.headers.get("cf-connecting-ip") ?? "");
        return env.TUNNEL.get(env.TUNNEL.idFromName(sub)).fetch(new Request(url, request));
      }

      return env.TUNNEL.get(env.TUNNEL.idFromName(sub)).fetch(request);
  }
}

function passthrough(): Policy {
  return {
    async connect() {
      return { action: "allow" };
    },
  };
}

// Only the connector must prove who it is; visitor traffic rides a tunnel an
// authenticated connector already opened. The JWT is verified once here, at
// connect, against the configured issuer's JWKS (cached per isolate).
const jwksCache = new Map<string, ReturnType<typeof createRemoteJWKSet>>();

function jwks(url: string): ReturnType<typeof createRemoteJWKSet> {
  let set = jwksCache.get(url);
  if (!set) {
    set = createRemoteJWKSet(new URL(url));
    jwksCache.set(url, set);
  }
  return set;
}

async function authenticate(
  request: Request,
  env: Env,
): Promise<{ userId: string; payload: Record<string, unknown> } | { error: Response }> {
  const issuer = env.ISSUER;
  if (!issuer) {
    return { error: text("identity provider not configured on this edge\n", 500) };
  }
  const header = request.headers.get("authorization") ?? "";
  const token = header.toLowerCase().startsWith("bearer ") ? header.slice(7).trim() : "";
  if (!token) {
    return { error: text("sign in to open a tunnel\n", 401) };
  }

  try {
    const { payload } = await jwtVerify(token, jwks(env.JWKS_URL ?? `${issuer}/api/auth/jwks`), {
      issuer,
    });
    if (!payload.sub) {
      throw new Error("no subject");
    }
    return { userId: payload.sub, payload: payload as Record<string, unknown> };
  } catch {
    return { error: text("your session is invalid or expired, sign in again\n", 401) };
  }
}

async function serveInstaller(): Promise<Response> {
  const res = await fetch(INSTALL_SCRIPT, { cf: { cacheTtl: 300, cacheEverything: true } });
  if (!res.ok) {
    return text("installer unavailable\n", 502);
  }
  return new Response(res.body, {
    status: 200,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "public, max-age=300",
    },
  });
}

function defaultLanding(env: Env): Response {
  return text(`neko edge running on ${domain(env)}\n`, 200);
}

function domain(env: Env): string {
  return env.DOMAIN ?? "neko.computer";
}

function subdomainOf(host: string, base: string): string | null {
  const h = host.split(":")[0];
  const suffix = `.${base}`;
  if (!h.endsWith(suffix)) return null;
  const label = h.slice(0, -suffix.length);
  if (!label || label.includes(".")) return null;
  return label;
}

function text(body: string, status: number): Response {
  return new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8" } });
}
