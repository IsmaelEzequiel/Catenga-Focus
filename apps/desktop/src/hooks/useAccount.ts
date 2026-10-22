import { useCallback, useEffect, useRef, useState } from "react";
import { cancelSignIn, clearAccountToken, getAccountToken, serverUrl, setAccountToken, signIn } from "../api";
import { exchangeCode, fetchMe, logout, type AccountUser } from "../lib/account";
import { createPkce } from "../lib/pkce";

export type AccountState =
  | { kind: "signedOut" }
  | { kind: "signingIn" }
  | { kind: "signedIn"; user: AccountUser }
  /** A token is stored but the server could not be reached; the app keeps working. */
  | { kind: "signedInOffline" }
  | { kind: "error"; message: string };

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Optional account: never blocks the rest of the app, and no account is a normal state. */
export function useAccount() {
  const [state, setState] = useState<AccountState>({ kind: "signedOut" });
  /** Bumped by every sign-in attempt and by cancel, so a stale attempt cannot overwrite the state. */
  const attempt = useRef(0);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const token = await getAccountToken();
      if (!token || cancelled) return;
      try {
        const me = await fetchMe(await serverUrl(), token);
        if (cancelled) return;
        if (me === "unauthorized") {
          await clearAccountToken();
          if (!cancelled) setState({ kind: "signedOut" });
        } else {
          setState({ kind: "signedIn", user: me });
        }
      } catch {
        // Offline or server down: keep the token and stay signed in.
        if (!cancelled) setState({ kind: "signedInOffline" });
      }
    })().catch(() => {
      // Keychain unavailable: behave as signed out.
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const startSignIn = useCallback(async () => {
    const mine = ++attempt.current;
    setState({ kind: "signingIn" });
    try {
      const { verifier, challenge } = await createPkce();
      const code = await signIn(challenge);
      const { token, user } = await exchangeCode(await serverUrl(), code, verifier);
      await setAccountToken(token);
      if (mine === attempt.current) setState({ kind: "signedIn", user });
    } catch (e) {
      if (mine !== attempt.current) return;
      setState(message(e) === "cancelled" ? { kind: "signedOut" } : { kind: "error", message: message(e) });
    }
  }, []);

  const cancel = useCallback(() => {
    attempt.current++;
    setState({ kind: "signedOut" });
    cancelSignIn().catch(() => {});
  }, []);

  const signOut = useCallback(async () => {
    attempt.current++;
    try {
      const token = await getAccountToken();
      if (token) {
        // Best effort: the local token is removed even when the server is unreachable.
        await logout(await serverUrl(), token).catch(() => {});
      }
      await clearAccountToken();
      setState({ kind: "signedOut" });
    } catch (e) {
      setState({ kind: "error", message: message(e) });
    }
  }, []);

  return { state, signIn: startSignIn, cancel, signOut };
}
