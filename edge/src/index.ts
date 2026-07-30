// Self-host entry: identity plus transport, no policy. Deployments that want
// name ownership, quotas, gating, or deny-lists supply a Policy and options
// through createEdge; see policy.ts and docs/self-hosting.md.
import { createEdge } from "./lib";

export { Limiter, Tunnel } from "./lib";

export default createEdge();
