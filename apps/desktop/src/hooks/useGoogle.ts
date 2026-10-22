import { useCallback, useEffect, useRef, useState } from "react";
import { googleCancelConnect, googleConnect, googleDisconnect, googleStatus, onProjectsChanged, type GoogleStatus } from "../api";
import { createPkce, randomState } from "../lib/pkce";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export const LOST_MESSAGE = "Google Calendar was disconnected — connect again";

/** Optional Google Calendar connection; never blocks the app, and "not connected" is a normal state. */
export function useGoogle() {
  const [status, setStatus] = useState<GoogleStatus | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Bumped by every connect attempt and by cancel, so a stale attempt cannot overwrite the state. */
  const attempt = useRef(0);
  const connectedRef = useRef(false);
  connectedRef.current = status?.connected ?? false;

  useEffect(() => {
    let cancelled = false;
    googleStatus().then(
      (s) => !cancelled && setStatus(s),
      () => {
        // Backend unavailable: behave as not configured.
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  /** Re-reads the status (no network); reports when the refresh token was dropped behind our back. */
  const recheck = useCallback(async () => {
    try {
      const s = await googleStatus();
      if (connectedRef.current && !s.connected) setError(LOST_MESSAGE);
      setStatus(s);
    } catch {
      // keep the current status
    }
  }, []);

  // The background calendar-list refresh can find a token without the new scope (needsReconnect).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onProjectsChanged(() => void recheck()).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {
        // No event bridge (tests, plain browser).
      },
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [recheck]);

  const connect = useCallback(async () => {
    const mine = ++attempt.current;
    setError(null);
    setConnecting(true);
    try {
      const { verifier, challenge } = await createPkce();
      const next = await googleConnect(challenge, verifier, randomState());
      if (mine !== attempt.current) return;
      setStatus(next);
      setConnecting(false);
    } catch (e) {
      if (mine !== attempt.current) return;
      setConnecting(false);
      if (message(e) !== "cancelled") setError(message(e));
    }
  }, []);

  const cancel = useCallback(() => {
    attempt.current++;
    setConnecting(false);
    googleCancelConnect().catch(() => {});
  }, []);

  const disconnect = useCallback(async () => {
    attempt.current++;
    setError(null);
    try {
      setStatus(await googleDisconnect());
    } catch (e) {
      setError(message(e));
    }
  }, []);

  return { status, connecting, error, connect, cancel, disconnect, recheck };
}
