/** In-memory fixed-window counter. Returns `hit(key)`: true while under the limit. */
export function createRateLimiter(
  limit: number,
  windowMs: number,
  now: () => number,
): (key: string) => boolean {
  const windows = new Map<string, { start: number; count: number }>();
  return (key) => {
    const t = now();
    for (const [k, w] of windows) if (t - w.start >= windowMs) windows.delete(k);
    const w = windows.get(key) ?? { start: t, count: 0 };
    windows.set(key, w);
    w.count += 1;
    return w.count <= limit;
  };
}
