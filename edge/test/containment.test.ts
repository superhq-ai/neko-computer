import { expect, test } from "bun:test";
import { configure, contain, containAsync, DEFAULT_LIMITS, allowAll } from "../src/policy";

// A tunnel must survive its own bugs. These pin the containment helpers the
// Durable Object's entry points rely on: they swallow the throw, report it,
// and never let it reach the runtime (where it would tear down the isolate and
// every tunnel on it).

test("a throwing handler is contained and reported", () => {
  const seen: string[] = [];
  configure(allowAll, DEFAULT_LIMITS, (where, e) => seen.push(`${where}:${(e as Error).message}`));

  const result = contain("tunnel.webSocketMessage", () => {
    throw new Error("bad frame");
  });

  expect(result).toBeUndefined();
  expect(seen).toEqual(["tunnel.webSocketMessage:bad frame"]);
});

test("a successful handler passes its value through untouched", () => {
  configure(allowAll, DEFAULT_LIMITS, () => {});
  expect(contain("x", () => 42)).toBe(42);
});

test("a rejecting async handler is contained and reported", async () => {
  const seen: string[] = [];
  configure(allowAll, DEFAULT_LIMITS, (where, e) => seen.push(`${where}:${(e as Error).message}`));

  const result = await containAsync("tunnel.alarm", async () => {
    throw new Error("storage gone");
  });

  expect(result).toBeUndefined();
  expect(seen).toEqual(["tunnel.alarm:storage gone"]);
});

test("containment survives a reporter that itself throws nothing useful", () => {
  configure(allowAll, DEFAULT_LIMITS, () => {});
  expect(() =>
    contain("x", () => {
      throw new Error("boom");
    }),
  ).not.toThrow();
});
