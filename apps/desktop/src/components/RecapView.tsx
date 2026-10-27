import { useEffect, useMemo, useRef, useState } from "react";
import { CheckIcon, ChevronRightIcon, ClipboardDocumentIcon } from "@heroicons/react/20/solid";
import type { Session, TimerStatus } from "../api";
import { fmtDuration, fmtDayShort } from "../lib/dashboard";
import { buildRecap, toMarkdown } from "../lib/recap";
import { fmtRange } from "../lib/search";
import { tagColorVar } from "../lib/tags";
import { fmtDateLabel, fmtDateShort } from "../lib/time";
import type { UiBlock } from "../hooks/useDay";
import { Button } from "./ui/Button";
import { ListRow } from "./ui/ListRow";
import "../card.css";
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
  onOpenBlock: (block: UiBlock) => void;
}

const COPIED_MS = 2000;

/** What got done on the day shown: finished sessions or notes, with time spent and the first line of each note. */
export function RecapView({ date, isToday, loaded, blocks, sessions, status, nowMs, tagColors, onOpenBlock }: Props) {
  // The running timer only moves the numbers once a minute.
  const minuteMs = Math.floor(nowMs / 60_000) * 60_000;
  const recap = useMemo(() => buildRecap(blocks, sessions, status, minuteMs, date), [blocks, sessions, status, minuteMs, date]);
  const [showOther, setShowOther] = useState(false);
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
  const doneKeys = new Set(recap.done.map((d) => d.key));
  const others = blocks.filter((b) => !doneKeys.has(b.key));

  return (
    <section className="recap" aria-label="Day recap">
      <div className="recap__scroll">
        <div className="recap__col">
          {loaded && recap.doneCount === 0 && recap.totalMinutes === 0 ? (
            <p className="recap__empty">
              {isToday ? "Nothing done yet today — start a block and it shows up here." : `Nothing was done on ${fmtDateShort(date)}.`}
            </p>
          ) : (
            loaded && (
              <>
                <div className="recap__total" aria-live="polite">
                  <div className="recap__summary">
                    <span className="recap__date">{heading}</span>
                    <span className="recap__big">{fmtDuration(recap.totalMinutes)}</span>
                    <span className="recap__sub">
                      focused · {recap.doneCount} {blocksWord} done
                    </span>
                  </div>
                  <Button
                    size="sm"
                    icon={copy === "copied" ? CheckIcon : ClipboardDocumentIcon}
                    onClick={() => void onCopy()}
                    disabled={!loaded || recap.doneCount === 0}
                  >
                    {copy === "copied" ? "Copied" : copy === "failed" ? "Couldn't copy" : "Copy"}
                  </Button>
                </div>
                <div className="card card--flush">
                  {recap.done.map((d) => (
                    <ListRow
                      key={d.key}
                      className="recap__row"
                      title={
                        <span className="recap__item">
                          <span className="recap__head">
                            <span className="recap__name">{d.title.trim() || "Untitled"}</span>
                            {d.tags.map((tag) => (
                              <span
                                key={tag}
                                className="recap__tag"
                                style={{ ["--tag" as string]: tagColorVar([tag], tagColors) ?? "var(--color-tag-none)" }}
                              >
                                <span className="recap__dot" />#{tag}
                              </span>
                            ))}
                          </span>
                          <b className="recap__spent">{d.minutes > 0 ? fmtDuration(d.minutes) : ""}</b>
                          <span className="recap__line">{d.firstLine}</span>
                          <span className="recap__time">{fmtRange(d.start, d.end)}</span>
                        </span>
                      }
                      onClick={() => {
                        const block = byKey.get(d.key);
                        if (block) onOpenBlock(block);
                      }}
                    />
                  ))}
                </div>
              </>
            )
          )}
          {loaded && recap.otherCount > 0 && (
            <div className="recap__other">
              <button
                type="button"
                className="recap__toggle"
                aria-expanded={showOther}
                aria-controls="recap-other"
                onClick={() => setShowOther((v) => !v)}
              >
                <ChevronRightIcon className="recap__chevron" aria-hidden="true" />
                {recap.otherCount} other {recap.otherCount === 1 ? "block" : "blocks"} planned
              </button>
              {showOther && (
                <div className="card card--flush" id="recap-other">
                  {others.map((b) => (
                    <ListRow
                      key={b.key}
                      title={<span className="recap__name">{b.title.trim() || "Untitled"}</span>}
                      meta={<span className="recap__time">{fmtRange(b.start, b.end)}</span>}
                      onClick={() => onOpenBlock(b)}
                    />
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
