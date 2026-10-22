import { describe, expect, it } from "vitest";
import { challengeFor, createPkce, randomState } from "./pkce";

describe("pkce", () => {
  it("challenge_matches_rfc7636_appendix_b", async () => {
    expect(await challengeFor("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")).toBe(
      "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
    );
  });

  it("verifier_is_43_base64url_chars", async () => {
    const { verifier } = await createPkce();
    expect(verifier).toMatch(/^[A-Za-z0-9_-]{43}$/);
  });

  it("challenge_is_43_base64url_chars_and_matches_verifier", async () => {
    const { verifier, challenge } = await createPkce();
    expect(challenge).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(challenge).toBe(await challengeFor(verifier));
  });

  it("pairs_differ_between_calls", async () => {
    expect((await createPkce()).verifier).not.toBe((await createPkce()).verifier);
  });
});

describe("randomState", () => {
  it("is_43_base64url_chars_and_differs_between_calls", () => {
    const a = randomState();
    expect(a).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(randomState()).not.toBe(a);
  });
});
