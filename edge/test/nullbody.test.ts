import { expect, test } from "bun:test";
import { forbidsBody } from "../src/tunnel";

// A browser reload sends validators and origins answer 304, which the Fetch
// spec forbids a body on; attaching the response stream to one throws a
// TypeError.

test("statuses that must not carry a body", () => {
  for (const s of [101, 204, 205, 304]) {
    expect(forbidsBody(s)).toBe(true);
  }
});

test("ordinary statuses still stream", () => {
  for (const s of [200, 201, 206, 301, 302, 400, 404, 500, 503]) {
    expect(forbidsBody(s)).toBe(false);
  }
});

// Note: the Workers runtime throws on `new Response(body, {status: 304})`
// while Bun accepts it, so that behavior is verified against the deployed
// edge (a conditional request must come back 304, not 500) rather than here.
