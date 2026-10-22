import { useEffect, useMemo, useRef, useState } from "react";
import { CheckIcon, ChevronLeftIcon, ClipboardDocumentIcon } from "@heroicons/react/20/solid";
import type { Session, TimerStatus } from "../api";
import { fmtDuration, fmtDayShort } from "../lib/dashboard";
import { buildRecap, toMarkdown } from "../lib/recap";
import { fmtRange } from "../lib/search";
import { tagColorVar } from "../lib/tags";
import { fmtDateLabel, fmtDateShort } from "../lib/time";
import type { UiBlock } from "../hooks/useDay";
import "../recap.css";

interface Props {
  date: string;
  isToday: boolean;
  loaded: boolean;
  blocks: UiBlock[];
  sessions: Session[];
  status: TimerStatus;
  nowMs: number;
  tagColors: Record<string, string>;
  onBack: () => void;
  onOpenBlock: (block: UiBlock) => void;
}

const COPIED_MS = 2000;

/** What got done on the day shown: finished sessions or notes, with time spent and the first line of each note. */
export function RecapView({ date, isToday, loaded, blocks, sessions, status, nowMs, tagColors, onBack, onOpenBlock }: Props) {
  // The running timer only moves the numbers once a minute.
  const minuteMs = Math.floor(nowMs / 60_000) * 60_000;
  const recap = useMemo(() => buildRecap(blocks, sessions, status, minuteMs, date), [blocks, sessions, status, minuteMs, date]);
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  const timeout = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timeout.current), []);

  const onCopy = async () => {
    try {
      await navigator.clipboard.writeText(toMarkdown(recap, fmtDayShort(date)));
      setCopy("copied");
    } catch {
      setCopy("failed");
    }
    clearTimeout(timeout.current);
    timeout.current = setTimeout(() => setCopy("idle"), COPIED_MS);
  };

  const heading = isToday ? "Today" : fmtDateLabel(date);
  const blocksWord = recap.doneCount === 1 ? "block" : "blocks";
  const byKey = new Map(blocks.map((b) => [b.key, b]));

  return (
    <section className="recap" aria-label="Day recap">
      <header className="recap__header">
        <button className="app__btn" onClick={onBack}>
          <ChevronLeftIcon className="app__icon" aria-hidden="true" />
          Day
        </button>
        <h2 className="recap__title">{heading}</h2>
        <button className="app__btn" onClick={() => void onCopy()} disabled={!loaded || recap.doneCount === 0}>
          {copy === "copied" ? <CheckIcon className="app__icon" aria-hidden="true" /> : <ClipboardDocumentIcon className="app__icon" aria-hidden="true" />}
          {copy === "copied" ? "Copied" : copy === "failed" ? "Couldn't copy" : "Copy"}
        </button>
      </header>
      <div className="recap__scroll">
        <div className="recap__col">
          {loaded && recap.doneCount === 0 && recap.totalMinutes === 0 ? (
            <p className="recap__empty">
              {isToday ? "Nothing done yet today — start a block and it shows up here." : `Nothing was done on ${fmtDateShort(date)}.`}
            </p>
          ) : (
            loaded && (
              <>
                <p className="recap__total" aria-live="polite">
                  <span className="recap__big">{fmtDuration(recap.totalMinutes)}</span> focused · {recap.doneCount} {blocksWord} done
                </p>
                <ul className="recap__list">
                  {recap.done.map((d) => (
                    <li key={d.key}>
                      <button
                        className="recap__row"
                        onClick={() => {
                          const block = byKey.get(d.key);
                          if (block) onOpenBlock(block);
                        }}
                      >
                        <span className="recap__time">{fmtRange(d.start, d.end)}</span>
                        <span className="recap__main">
                          <span className="recap__name">{d.title.trim() || "Untitled"}</span>
                          {d.tags.map((tag) => (
                            <span key={tag} className="recap__chip">
                              <span className="recap__dot" style={{ background: tagColorVar([tag], tagColors) ?? "var(--color-line)" }} />#{tag}
                            </span>
                          ))}
                        </span>
                        <span className="recap__spent">{d.minutes > 0 ? fmtDuration(d.minutes) : ""}</span>
                        {d.firstLine && <span className="recap__line">{d.firstLine}</span>}
                      </button>
                    </li>
                  ))}
                </ul>
              </>
            )
          )}
          {loaded && recap.otherCount > 0 && (
            <p className="recap__other">
              {recap.otherCount} other {recap.otherCount === 1 ? "block" : "blocks"} planned
            </p>
          )}
        </div>
      </div>
    </section>
  );
}
