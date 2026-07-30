export interface Env {
  TUNNEL: DurableObjectNamespace;
  LIMITER: DurableObjectNamespace;
  DENYLIST: KVNamespace;
  // Identity provider. ISSUER is required; keys are fetched from
  // JWKS_URL or `${ISSUER}/api/auth/jwks` (the better-auth convention).
  ISSUER?: string;
  JWKS_URL?: string;
  // Shared HS256 secret for private tunnel visitor tokens, minted by the
  // identity provider at TUNNEL_AUTH_URL or `${ISSUER}/api/tunnel-auth`
  // (the same convention shape as JWKS_URL). Absent, a private tunnel
  // answers 503 rather than serving unauthenticated visitors.
  TUNNEL_AUTH_SECRET?: string;
  TUNNEL_AUTH_URL?: string;
  // The domain this edge serves tunnels under.
  DOMAIN?: string;
}
