export interface AutosaveTimerOptions {
  /** Quiet period after the last `touch()` before firing. */
  delayMs?: number;
  /** Upper bound from the first unfired `touch()`, so continuous typing still saves. */
  maxWaitMs?: number;
  now?: () => number;
}

export interface AutosaveTimer {
  touch(): void;
  /** Fires now if a fire is pending; otherwise does nothing. */
  flush(): void;
  cancel(): void;
}

export function createAutosaveTimer(fire: () => void, options: AutosaveTimerOptions = {}): AutosaveTimer {
  const { delayMs = 600, maxWaitMs = 3000, now = Date.now } = options;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let firstTouch = 0;

  const clear = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  const run = () => {
    clear();
    fire();
  };

  return {
    touch() {
      const t = now();
      if (timer === null) firstTouch = t;
      clear();
      timer = setTimeout(run, Math.max(0, Math.min(delayMs, firstTouch + maxWaitMs - t)));
    },
    flush() {
      if (timer !== null) run();
    },
    cancel: clear,
  };
}
