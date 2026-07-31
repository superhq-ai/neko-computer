import type { Env } from "./env";

// Fixed-window counter. The window and cap come from the caller, so one class
// backs both the per-IP creation rate and deployment-defined daily caps.
export class Limiter {
  private count = 0;
  private windowStart = 0;

  constructor(_ctx: DurableObjectState, _env: Env) {}

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const window = Number(url.searchParams.get("window") ?? "60000");

    const now = Date.now();
    if (now - this.windowStart > window) {
      this.windowStart = now;
      this.count = 0;
    }

    // Read without spending: what the current window has counted so far.
    if (url.pathname === "/peek") {
      return Response.json({ count: this.count });
    }

    const max = Number(url.searchParams.get("max") ?? "30");
    this.count += 1;
    return new Response(null, { status: this.count > max ? 429 : 200 });
  }
}
