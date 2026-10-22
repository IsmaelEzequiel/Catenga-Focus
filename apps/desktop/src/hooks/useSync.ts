import { useCallback, useEffect, useState } from "react";
import { onSyncStatus, syncNow, syncStatus, type SyncStatus } from "../api";
import type { AccountState } from "./useAccount";

/** Sync status from the backend; runs a sync as soon as the account is signed in. */
export function useSync(account: AccountState) {
  const [status, setStatus] = useState<SyncStatus | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onSyncStatus(setStatus)
      .then((u) => (cancelled ? u() : (unlisten = u)))
      .catch(() => {});
    syncStatus()
      .then((s) => !cancelled && setStatus(s))
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const signedIn = account.kind === "signedIn";
  useEffect(() => {
    // After a sign-in (a new computer pulls everything, one with data pushes it).
    if (signedIn) syncNow().then(setStatus).catch(() => {});
  }, [signedIn]);

  const now = useCallback(() => syncNow().then(setStatus).catch(() => {}), []);
  return { status, syncNow: now };
}
