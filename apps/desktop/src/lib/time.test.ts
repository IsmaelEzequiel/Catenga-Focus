import { describe, expect, it } from "vitest";
import {
  PX_PER_MIN,
  addDays,
  clampMove,
  clampResize,
  createRange,
  fmtMinutes,
  minuteFromY,
  snap,
  toDateString,
} from "./time";

describe("time", () => {
  it("snap_rounds_to_15", () => {
    expect(snap(0)).toBe(0);
    expect(snap(7)).toBe(0);
    expect(snap(8)).toBe(15);
    expect(snap(22)).toBe(15);
    expect(snap(23)).toBe(30);
    expect(snap(-8)).toBe(-15);
  });

  it("minuteFromY_respects_scale", () => {
    expect(minuteFromY(0)).toBe(0);
    expect(minuteFromY(64)).toBeCloseTo(60);
    expect(minuteFromY(16)).toBeCloseTo(15);
    expect(minuteFromY(PX_PER_MIN * 90)).toBeCloseTo(90);
  });

  it("fmtMinutes_pads_and_handles_1440", () => {
    expect(fmtMinutes(0)).toBe("00:00");
    expect(fmtMinutes(545)).toBe("09:05");
    expect(fmtMinutes(1380)).toBe("23:00");
    expect(fmtMinutes(1440)).toBe("24:00");
  });

  it("clampMove_stops_at_midnight_and_zero", () => {
    expect(clampMove(60, 120, -500)).toEqual({ start: 0, end: 60 });
    expect(clampMove(1300, 1380, 500)).toEqual({ start: 1360, end: 1440 });
  });

  it("clampMove_keeps_duration", () => {
    const r = clampMove(540, 630, 45);
    expect(r).toEqual({ start: 585, end: 675 });
    expect(r.end - r.start).toBe(90);
  });

  it("clampResize_min_15", () => {
    expect(clampResize("start", 540, 600, 700)).toEqual({ start: 585, end: 600 });
    expect(clampResize("end", 540, 600, 500)).toEqual({ start: 540, end: 555 });
  });

  it("clampResize_bounds", () => {
    expect(clampResize("start", 540, 600, -30)).toEqual({ start: 0, end: 600 });
    expect(clampResize("end", 540, 600, 2000)).toEqual({ start: 540, end: 1440 });
  });

  it("createRange_floors_start_and_ceils_end", () => {
    expect(createRange(550, 620)).toEqual({ start: 540, end: 630 });
    expect(createRange(620, 550)).toEqual({ start: 540, end: 630 });
    expect(createRange(545, 545)).toEqual({ start: 540, end: 555 });
    expect(createRange(1439, 2000)).toEqual({ start: 1425, end: 1440 });
  });

  it("addDays_crosses_month_and_year", () => {
    expect(addDays("2026-12-31", 1)).toBe("2027-01-01");
    expect(addDays("2027-01-01", -1)).toBe("2026-12-31");
    expect(addDays("2026-02-28", 1)).toBe("2026-03-01");
    expect(addDays("2024-02-28", 1)).toBe("2024-02-29");
  });

  it("addDays_dst_safe", () => {
    let d = "2026-03-01";
    for (let i = 0; i < 400; i++) d = addDays(d, 1);
    expect(d).toBe("2027-04-05");
    for (let i = 0; i < 400; i++) d = addDays(d, -1);
    expect(d).toBe("2026-03-01");
  });

  it("toDateString_local_not_utc", () => {
    expect(toDateString(new Date(2026, 9, 20, 0, 5))).toBe("2026-10-20");
    expect(toDateString(new Date(2026, 9, 20, 23, 55))).toBe("2026-10-20");
  });
});
