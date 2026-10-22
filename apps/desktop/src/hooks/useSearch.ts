import { useEffect, useRef, useState } from "react";
import { onIndexStatus, search, type SearchResponse } from "../api";

const DEBOUNCE_MS = 150;

/**
 * Debounced search while mounted. Each request carries an id so a slow, stale
 * response never replaces a newer one; results are refreshed once an index
 * build that was still running finishes.
 */
export function useSearch(query: string) {
  const [response, setResponse] = useState<SearchResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [readyTick, setReadyTick] = useState(0);
  const latest = useRef(0);
  const indexing = useRef(false);
  indexing.current = response?.indexing ?? false;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onIndexStatus((status) => {
      if (status.state === "ready" && indexing.current) setReadyTick((t) => t + 1);
    }).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no live refresh; the next keystroke still searches
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const id = ++latest.current;
    setLoading(true);
    const timer = setTimeout(
      () => {
        search(query).then(
          (r) => {
            if (id !== latest.current) return;
            setResponse(r);
            setError(null);
            setLoading(false);
          },
          (e) => {
            if (id !== latest.current) return;
            setResponse(null);
            setError(String(e));
            setLoading(false);
          },
        );
      },
      query.trim() === "" ? 0 : DEBOUNCE_MS,
    );
    return () => clearTimeout(timer);
  }, [query, readyTick]);

  return { response, loading, error };
}
