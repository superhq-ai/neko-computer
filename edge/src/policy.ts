// The policy seam. The open edge owns identity verification and the tunnel
// transport; everything that decides what an identity may do lives behind
// this interface. A deployment plugs its policy in via createEdge; without
// one, everything an authenticated user asks for is allowed, including
// replacing another account's live connector. Deployments that need name
// ownership, quotas, or abuse limits implement them here.

export interface ConnectContext {
  userId: string;
  sub: string;
  // The connector asked to keep this name (--subdomain).
  wantsClaim: boolean;
  // The connector asked for a private tunnel (--private) scoped to org.
  wantsPrivate: boolean;
  org: string | null;
  // Account of the currently attached connector, null when none. A policy
  // that does not want live tunnels replaced across accounts denies here.
  liveUser: string | null;
  // Owning workspace of the attached connector. Null on connectors opened by
  // a pre-workspace client before their next reconnect.
  liveOrg: string | null;
  // Verified JWT claims from the configured identity provider.
  claims: Record<string, unknown>;
  ip: string;
}

export type ConnectVerdict =
  // access restricts visitors to holders of a tunnel-auth token for org;
  // the transport enforces it, the policy decides it.
  | { action: "allow"; owner?: { org: string }; access?: { org: string } }
  // The tunnel opens, but visitors are redirected instead of proxied.
  | { action: "gate"; redirect: string; owner?: { org: string } }
  | { action: "deny"; status: number; message: string };

export interface Policy<E = unknown> {
  // Called from the Tunnel object before the connector socket is accepted.
  connect(ctx: ConnectContext, env: E): Promise<ConnectVerdict>;
  // The connector for sub closed; whatever connect counted can be released.
  closed?(ctx: { userId: string; org: string | null; sub: string }, env: E): Promise<void>;
}

export const allowAll: Policy = {
  async connect() {
    return { action: "allow" };
  },
};

// Transport limits. Timeouts and memory guards default on, since a relay
// without them leaks streams; volume caps default off and are a deployment
// decision.
export interface TransportLimits {
  headTimeoutMs: number;
  idleTimeoutMs: number;
  maxLifetimeMs: number;
  requestBodyCap: number;
  responseOverflow: number;
  highWater: number;
  // Token bucket per tunnel; undefined means unthrottled.
  bucket?: { burst: number; perSec: number };
  // Concurrent stream and websocket caps; undefined means unbounded.
  maxConcurrent?: number;
  maxWs?: number;
}

export const DEFAULT_LIMITS: TransportLimits = {
  headTimeoutMs: 30_000,
  idleTimeoutMs: 120_000,
  maxLifetimeMs: 600_000,
  requestBodyCap: 100_000_000,
  responseOverflow: 8_000_000,
  highWater: 1_000_000,
};

let currentPolicy: Policy = allowAll;
let currentLimits: TransportLimits = DEFAULT_LIMITS;
// Durable Object handlers never run the worker's fetch, so they must hand
// their own env in for a report to be persistable.
export type Reporter = (where: string, error: unknown, env?: unknown) => void;

let currentReporter: Reporter = () => {};

export function configure(policy: Policy, limits?: Partial<TransportLimits>, reporter?: Reporter) {
  currentPolicy = policy;
  currentLimits = { ...DEFAULT_LIMITS, ...limits };
  if (reporter) currentReporter = reporter;
}

/// Report a failure without letting it escape. A tunnel must survive its own
/// bugs: one broken frame or handler degrades one request, never the actor.
export function contain<T>(where: string, fn: () => T, env?: unknown): T | undefined {
  try {
    return fn();
  } catch (e) {
    currentReporter(where, e, env);
    return undefined;
  }
}

export async function containAsync<T>(
  where: string,
  fn: () => Promise<T>,
  env?: unknown,
): Promise<T | undefined> {
  try {
    return await fn();
  } catch (e) {
    currentReporter(where, e, env);
    return undefined;
  }
}

export function report(where: string, error: unknown, env?: unknown) {
  currentReporter(where, error, env);
}

export function activePolicy(): Policy {
  return currentPolicy;
}

export function activeLimits(): TransportLimits {
  return currentLimits;
}
