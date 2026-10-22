import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAutosaveTimer } from "./autosaveTimer";

describe("autosaveTimer", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("fires_after_delay", () => {
    const fire = vi.fn();
    const t = createAutosaveTimer(fire);
    t.touch();
    vi.advanceTimersByTime(599);
    expect(fire).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fire).toHaveBeenCalledTimes(1);
  });

  it("touch_restarts_the_delay", () => {
    const fire = vi.fn();
    const t = createAutosaveTimer(fire);
    t.touch();
    vi.advanceTimersByTime(500);
    t.touch();
    vi.advanceTimersByTime(500);
    expect(fire).not.toHaveBeenCalled();
    vi.advanceTimersByTime(100);
    expect(fire).toHaveBeenCalledTimes(1);
  });

  it("max_wait_fires_during_continuous_typing", () => {
    const fire = vi.fn();
    const t = createAutosaveTimer(fire);
    for (let i = 0; i < 40; i++) {
      t.touch();
      vi.advanceTimersByTime(100);
    }
    expect(fire).toHaveBeenCalledTimes(1);
    // The window restarts after a fire.
    t.touch();
    vi.advanceTimersByTime(600);
    expect(fire).toHaveBeenCalledTimes(2);
  });

  it("flush_fires_immediately_once", () => {
    const fire = vi.fn();
    const t = createAutosaveTimer(fire);
    t.touch();
    t.flush();
    expect(fire).toHaveBeenCalledTimes(1);
    t.flush();
    vi.advanceTimersByTime(5000);
    expect(fire).toHaveBeenCalledTimes(1);
  });

  it("flush_when_idle_is_noop", () => {
    const fire = vi.fn();
    createAutosaveTimer(fire).flush();
    expect(fire).not.toHaveBeenCalled();
  });

  it("cancel_prevents_fire", () => {
    const fire = vi.fn();
    const t = createAutosaveTimer(fire);
    t.touch();
    t.cancel();
    vi.advanceTimersByTime(5000);
    t.flush();
    expect(fire).not.toHaveBeenCalled();
  });
});
