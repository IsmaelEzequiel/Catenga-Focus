/** PKCE (RFC 7636) for the browser sign-in handoff, S256 method. */

export interface Pkce {
  verifier: string;
  challenge: string;
}

function base64url(bytes: Uint8Array): string {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** base64url(SHA-256(verifier)) */
export async function challengeFor(verifier: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  return base64url(new Uint8Array(digest));
}

/** A fresh pair: verifier = 32 random bytes as base64url (43 chars). */
export async function createPkce(): Promise<Pkce> {
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
  return { verifier, challenge: await challengeFor(verifier) };
}

/** OAuth `state`: 32 random bytes as base64url. */
export function randomState(): string {
  return base64url(crypto.getRandomValues(new Uint8Array(32)));
}
