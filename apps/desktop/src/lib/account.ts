/** Plain fetch wrappers for the account API (see `docs/plans/M4-account.md` §1.5). */

export interface AccountUser {
  name: string;
  email: string;
}

export interface TokenResult {
  token: string;
  user: AccountUser;
}

/** The server's `{ error }` message when it sent one, else a status-based fallback. */
async function failure(response: Response): Promise<Error> {
  try {
    const body: unknown = await response.json();
    if (body && typeof body === "object" && "error" in body && typeof body.error === "string") {
      return new Error(body.error);
    }
  } catch {
    // not JSON; use the fallback below
  }
  return new Error(`Server error (${response.status})`);
}

const bearer = (token: string) => ({ Authorization: `Bearer ${token}` });

/** Trades the one-time code from the loopback callback for an app token. */
export async function exchangeCode(server: string, code: string, verifier: string): Promise<TokenResult> {
  const response = await fetch(`${server}/api/app/token`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code, code_verifier: verifier }),
  });
  if (!response.ok) throw await failure(response);
  return (await response.json()) as TokenResult;
}

/** The signed-in user, or "unauthorized" when the token is no longer valid. Throws when offline. */
export async function fetchMe(server: string, token: string): Promise<AccountUser | "unauthorized"> {
  const response = await fetch(`${server}/api/me`, { headers: bearer(token) });
  if (response.status === 401) return "unauthorized";
  if (!response.ok) throw await failure(response);
  return (await response.json()) as AccountUser;
}

/** Revokes the token on the server. Callers treat failure as non-fatal. */
export async function logout(server: string, token: string): Promise<void> {
  const response = await fetch(`${server}/api/logout`, { method: "POST", headers: bearer(token) });
  if (!response.ok) throw await failure(response);
}
