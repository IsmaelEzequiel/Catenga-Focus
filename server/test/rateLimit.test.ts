import { describe, expect, it } from "vitest";
import { createRateLimiter } from "../src/rateLimit.js";

describe("rate limiter", () => {
  it("allows up to the limit per key within a window, then resets", () => {
    let now = 0;
    const hit = createRateLimiter(2, 1000, () => now);
    expect(hit("a")).toBe(true);
    expect(hit("a")).toBe(true);
    expect(hit("a")).toBe(false);
    expect(hit("b")).toBe(true);
    now = 1000;
    expect(hit("a")).toBe(true);
  });
});
