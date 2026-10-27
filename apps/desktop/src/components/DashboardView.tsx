import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import {
  dashboard,
  onIndexStatus,
  onSessionsChanged,
  onTimerChanged,
  type DashRange,
  type Dashboard,
  type HistoryKind,
} from "../api";
import {
  barPercent,
  cellLabel,
  countsSessionsTwice,
  defaultOpenTag,
  fmtDuration,
  habitCellLabel,
  heatBucket,
  heatLayout,
  isValidRange,
  RANGES,
} from "../lib/dashboard";
import { tagColorVar } from "../lib/tags";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import { StatTile } from "./ui/StatTile";
import "../card.css";
import "../segmented.css";
import "../dashboard.css";

const RANGE_KEY = "catenga.dashboardRange";
const WEEKDAY_LABELS = ["Mon", "", "Wed", "", "Fri", "", ""];

function loadRange(): DashRange {
  try {
    const v = localStorage.getItem(RANGE_KEY);
    return isValidRange(v) ? v : "30d";
  } catch {
    return "30d";
  }
}

interface Props {
  tagColors: Record<string, string>;
  onOpenDay: (date: string) => void;
  onOpenHistory: (by: HistoryKind, value: string) => void;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** Hours per title and tag, streaks and the habit tracker. */
export function DashboardView({ tagColors, onOpenDay, onOpenHistory }: Props) {
  const [range, setRange] = useState<DashRange>(loadRange);
  const [data, setData] = useState<Dashboard | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [hover, setHover] = useState<string | null>(null);
  // Tag keys the user toggled; until then only the first tag is open.
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  const latest = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const unlisten: (() => void)[] = [];
    const keep = (p: Promise<() => void>) =>
      p.then(
        (fn) => (cancelled ? fn() : unlisten.push(fn)),
        () => {}, // no live refresh; reopening the view still loads
      );
    keep(onIndexStatus((s) => s.state === "ready" && setTick((t) => t + 1)));
    keep(onTimerChanged(() => setTick((t) => t + 1)));
    keep(onSessionsChanged(() => setTick((t) => t + 1)));
    return () => {
      cancelled = true;
      unlisten.forEach((fn) => fn());
    };
  }, []);

  useEffect(() => {
    const id = ++latest.current;
    dashboard(range).then(
      (d) => {
        if (id !== latest.current) return;
        setData(d);
        setError(null);
      },
      (e) => {
        if (id !== latest.current) return;
        setError(String(e));
      },
    );
  }, [range, tick]);

  const pick = (next: DashRange) => {
    setRange(next);
    try {
      localStorage.setItem(RANGE_KEY, next);
    } catch {
      // not remembered; still works
    }
  };

  const layout = useMemo(() => heatLayout(data?.calendar ?? []), [data?.calendar]);
  const maxTag = Math.max(0, ...(data?.tags ?? []).map((t) => t.minutes));
  const firstTag = defaultOpenTag(data?.tags ?? []);
  const isOpen = (key: string) => toggled[key] ?? key === firstTag;
  const twice = data ? countsSessionsTwice(data.tags, data.totals.sessions) : false;
  const empty =
    data !== null &&
    data.totals.minutes === 0 &&
    data.habits.length === 0 &&
    !data.calendar.some((c) => c.done);

  return (
    <section className="dash" aria-label="Dashboard">
      <div className="dash__scroll">
        <div className="dash__inner">
          <header className="dash__header">
            <h2 className="dash__title">Dashboard</h2>
            <div className="seg" role="group" aria-label="Range">
              {RANGES.map((r) => (
                <button
                  key={r.value}
                  type="button"
                  className="seg__item"
                  aria-pressed={range === r.value}
                  onClick={() => pick(r.value)}
                >
                  {r.label}
                </button>
              ))}
            </div>
          </header>

          <div className="dash__status" role="status" aria-live="polite">
            {data === null && !error && <span>Loading…</span>}
            {data?.indexing && <span>Indexing… numbers may be incomplete</span>}
          </div>
          {error && (
            <Banner
              tone="error"
              variant="inline"
              actions={
                <Button size="sm" onClick={() => setTick((t) => t + 1)}>
                  Try again
                </Button>
              }
            >
              Couldn't load your stats.
            </Banner>
          )}

          {data && empty && (
            <div className="dash__empty">
              <img src="/mark.svg" width={56} height={56} alt="" />
              <p>Start a timer or write a note on a block — your stats will grow here.</p>
            </div>
          )}

          {data && !empty && (
            <>
              <div className="dash__tiles">
                <StatTile label="Total time" value={fmtDuration(data.totals.minutes)} />
                <StatTile label="Sessions" value={String(data.totals.sessions)} />
                <StatTile label="Active days" value={String(data.totals.activeDays)} />
                <StatTile label="Current streak" value={plural(data.totals.currentStreak, "day", "days")} />
                <StatTile label="Best streak" value={plural(data.totals.longestStreak, "day", "days")} />
              </div>

              <section className="card dash__card" aria-label="Year">
                <div className="card__head">
                  <h3 className="card__title">Last 12 months</h3>
                </div>
                <div className="card__body">
                <div className="heat">
                  <div className="heat__scroll">
                    <div className="heat__grid" style={{ ["--cols" as string]: layout.columns.length }}>
                      <div className="heat__months" aria-hidden="true">
                        {layout.months.map((m) => (
                          <span key={m.column} style={{ gridColumn: m.column + 1 }}>
                            {m.label}
                          </span>
                        ))}
                      </div>
                      <div className="heat__days" aria-hidden="true">
                        {WEEKDAY_LABELS.map((d, i) => (
                          <span key={i}>{d}</span>
                        ))}
                      </div>
                      <div className="heat__cells">
                        {layout.columns.map((col, ci) =>
                          col.map((cell, ri) =>
                            cell === null ? (
                              <span key={`${ci}:${ri}`} className="heat__cell heat__cell--pad" aria-hidden="true" />
                            ) : (
                              <button
                                key={cell.date}
                                className={`heat__cell${cell.date === data.to ? " heat__cell--today" : ""}`}
                                data-level={heatBucket(cell.minutes)}
                                data-note={cell.done && cell.minutes === 0 ? "true" : undefined}
                                aria-label={cellLabel(cell.date, cell.minutes)}
                                onClick={() => onOpenDay(cell.date)}
                                onMouseEnter={() => setHover(cellLabel(cell.date, cell.minutes))}
                                onFocus={() => setHover(cellLabel(cell.date, cell.minutes))}
                                onMouseLeave={() => setHover(null)}
                                onBlur={() => setHover(null)}
                              />
                            ),
                          ),
                        )}
                      </div>
                    </div>
                  </div>
                  <div className="heat__foot">
                    <span className="heat__readout" aria-hidden="true">
                      {hover ?? "Click a day to open it"}
                    </span>
                    <span className="heat__legend" aria-hidden="true">
                      Less
                      {[0, 1, 2, 3, 4].map((l) => (
                        <i key={l} className="heat__cell heat__cell--legend" data-level={l} />
                      ))}
                      More
                    </span>
                  </div>
                </div>
                </div>
              </section>

              <section className="card dash__card" aria-label="Habits">
                <div className="card__head">
                  <h3 className="card__title">
                    Habits <span className="dash__hint">last 30 days</span>
                  </h3>
                </div>
                <div className="card__body">
                {data.habits.length === 0 ? (
                  <p className="dash__none">No habits yet — they appear after a few days of timers or notes.</p>
                ) : (
                  <ul className="habits">
                    {data.habits.map((h) => (
                      <li key={h.key} className="habit">
                        <button className="habit__name" onClick={() => onOpenHistory("title", h.label)} title={h.label}>
                          {h.label}
                        </button>
                        <span className="habit__strip">
                          {h.days.map((d) => (
                            <button
                              key={d.date}
                              className={`habit__cell${d.date === data.to ? " habit__cell--today" : ""}`}
                              data-state={d.minutes > 0 || (d.done && !d.note) ? "done" : d.note ? "note" : "none"}
                              aria-label={`${h.label}, ${habitCellLabel(d)}`}
                              title={habitCellLabel(d)}
                              onClick={() => onOpenDay(d.date)}
                            />
                          ))}
                        </span>
                        <span className="habit__streak">
                          <b>{h.currentStreak}</b> · best {h.longestStreak}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                <p className="habit__key" aria-hidden="true">
                  <i className="habit__cell" data-state="done" /> timer
                  <i className="habit__cell" data-state="note" /> note only
                  <i className="habit__cell" data-state="none" /> nothing
                </p>
                </div>
              </section>

              <section className="card dash__card" aria-label="Time by tag">
                <div className="card__head">
                  <h3 className="card__title">Time by tag</h3>
                </div>
                <div className="card__body">
                {data.tags.length === 0 ? (
                  <p className="dash__none">No timer sessions in this range.</p>
                ) : (
                  <ul className="bars">
                    {data.tags.map((t, i) => {
                      const untagged = t.key === "";
                      const open = isOpen(t.key);
                      const name = untagged ? "Untagged" : `#${t.label}`;
                      const panel = `tag-titles-${i}`;
                      return (
                        <li key={t.key} className="tagrow">
                          <div className="tagrow__head">
                            <button
                              className="tagrow__toggle"
                              aria-expanded={open}
                              aria-controls={panel}
                              aria-label={`${open ? "Collapse" : "Expand"} ${name}`}
                              onClick={() => setToggled((prev) => ({ ...prev, [t.key]: !open }))}
                            >
                              <ChevronRightIcon className="tagrow__chevron" aria-hidden="true" />
                            </button>
                            <div className="bar bar--tag">
                              {untagged ? (
                                <span className="bar__label bar__label--muted">{name}</span>
                              ) : (
                                <button
                                  className="bar__label bar__link"
                                  onClick={() => onOpenHistory("tag", t.label)}
                                  title={`History for ${name}`}
                                >
                                  {name}
                                </button>
                              )}
                              <span className="bar__value">{fmtDuration(t.minutes)}</span>
                              <span className="bar__track">
                                <span
                                  className="bar__fill"
                                  style={{
                                    width: `${barPercent(t.minutes, maxTag)}%`,
                                    background: untagged
                                      ? "var(--color-ink-muted)"
                                      : (tagColorVar([t.label], tagColors) ?? "var(--color-sun)"),
                                    animationDelay: `${Math.min(i, 10) * 30}ms`,
                                  }}
                                />
                              </span>
                              <span className="bar__meta">{plural(t.sessions, "session", "sessions")}</span>
                            </div>
                          </div>
                          <div className="tagrow__panel" data-open={open} id={panel} role="group" aria-label={`${name} titles`}>
                            <ul className="tagrow__titles">
                              {t.titles.map((ti) => (
                                <li key={ti.key} className="subbar">
                                  {ti.key === "" ? (
                                    <span className="subbar__label subbar__label--muted">Untitled</span>
                                  ) : (
                                    <button
                                      className="subbar__label subbar__link"
                                      onClick={() => onOpenHistory("title", ti.label)}
                                      title={ti.label}
                                    >
                                      {ti.label}
                                    </button>
                                  )}
                                  <span className="subbar__value">{fmtDuration(ti.minutes)}</span>
                                  <span className="subbar__track">
                                    <span
                                      className="subbar__fill"
                                      style={{
                                        width: `${barPercent(ti.minutes, t.minutes)}%`,
                                        background: untagged
                                          ? "var(--color-ink-muted)"
                                          : `color-mix(in srgb, ${tagColorVar([t.label], tagColors) ?? "var(--color-sun)"} 45%, var(--color-surface))`,
                                      }}
                                    />
                                  </span>
                                  <span className="subbar__meta">{plural(ti.sessions, "session", "sessions")}</span>
                                </li>
                              ))}
                            </ul>
                          </div>
                        </li>
                      );
                    })}
                  </ul>
                )}
                {twice && <p className="dash__note">Sessions with several tags count under each tag.</p>}
                </div>
              </section>
            </>
          )}
        </div>
      </div>
    </section>
  );
}
