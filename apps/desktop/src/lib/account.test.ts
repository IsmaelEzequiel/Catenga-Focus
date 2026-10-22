import { afterEach, describe, expect, it, vi } from "vitest";
import { exchangeCode, fetchMe, logout } from "./account";

const SERVER = "http://localhost:8080";

function stubFetch(impl: (url: string, init?: RequestInit) => Promise<Response> | Response) {
  const fn = vi.fn(async (url: string, init?: RequestInit) => impl(url, init));
  vi.stubGlobal("fetch", fn);
  return fn;
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

afterEach(() => vi.unstubAllGlobals());

describe("exchangeCode", () => {
  it("posts_code_and_verifier_and_returns_token_and_user", async () => {
    const fetch = stubFetch(() => json({ token: "t0k", user: { name: "Ana", email: "ana@x.com" } }));
    const result = await exchangeCode(SERVER, "the-code", "the-verifier");
    expect(result).toEqual({ token: "t0k", user: { name: "Ana", email: "ana@x.com" } });
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe(`${SERVER}/api/app/token`);
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ code: "the-code", code_verifier: "the-verifier" });
  });

  it("throws_the_server_error_message_on_400", async () => {
    stubFetch(() => json({ error: "Invalid or expired code" }, 400));
    await expect(exchangeCode(SERVER, "c", "v")).rejects.toThrow("Invalid or expired code");
  });

  it("throws_generic_message_when_error_body_is_not_json", async () => {
    stubFetch(() => new Response("oops", { status: 502 }));
    await expect(exchangeCode(SERVER, "c", "v")).rejects.toThrow(/502/);
  });
});

describe("fetchMe", () => {
  it("returns_user_with_bearer_header", async () => {
    const fetch = stubFetch(() => json({ name: "Ana", email: "ana@x.com" }));
    expect(await fetchMe(SERVER, "t0k")).toEqual({ name: "Ana", email: "ana@x.com" });
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe(`${SERVER}/api/me`);
    expect(new Headers(init?.headers).get("Authorization")).toBe("Bearer t0k");
  });

  it("returns_unauthorized_on_401", async () => {
    stubFetch(() => new Response(null, { status: 401 }));
    expect(await fetchMe(SERVER, "t0k")).toBe("unauthorized");
  });

  it("throws_on_server_error_so_the_token_is_kept", async () => {
    stubFetch(() => new Response(null, { status: 500 }));
    await expect(fetchMe(SERVER, "t0k")).rejects.toThrow();
  });

  it("throws_on_network_error", async () => {
    stubFetch(() => {
      throw new TypeError("Failed to fetch");
    });
    await expect(fetchMe(SERVER, "t0k")).rejects.toThrow();
  });
});

describe("logout", () => {
  it("posts_with_bearer_header", async () => {
    const fetch = stubFetch(() => new Response(null, { status: 204 }));
    await logout(SERVER, "t0k");
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe(`${SERVER}/api/logout`);
    expect(init?.method).toBe("POST");
    expect(new Headers(init?.headers).get("Authorization")).toBe("Bearer t0k");
  });
});
