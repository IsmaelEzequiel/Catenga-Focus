import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { MagnifyingGlassIcon } from "@heroicons/react/20/solid";
import type { HistoryKind, SearchHit } from "../api";
import { useSearch } from "../hooks/useSearch";
import {
  buildPaletteItems,
  fmtRange,
  stepIndex,
  type PaletteItem,
} from "../lib/search";
import { tagColorVar } from "../lib/tags";
import { fmtDateShort } from "../lib/time";
import { Highlighted } from "./Highlighted";
import "../palette.css";

interface Props {
  tagColors: Record<string, string>;
  onClose: () => void;
  onOpenBlock: (hit: SearchHit) => void;
  onOpenHistory: (by: HistoryKind, value: string) => void;
}

const LIST_ID = "palette-list";
const optionId = (i: number) => `palette-option-${i}`;
const BROWSE_LIMIT = 5;
type Group = "titles" | "tags" | "notes";
const groupOf = (item: PaletteItem): Group =>
  item.kind === "title" ? "titles" : item.kind === "tag" ? "tags" : "notes";

/** Note lines come back as a markdown snippet; show them on one line joined with " · ". */
const oneLine = (s: string) =>
  s.replace(/^\s*-\s+/, "").replace(/\s*\n+\s*(-\s+)?/g, " · ");

export function CommandPalette({
  tagColors,
  onClose,
  onOpenBlock,
  onOpenHistory,
}: Props) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const { response, loading, error } = useSearch(query);
  const [expanded, setExpanded] = useState<ReadonlySet<Group>>(new Set());
  const all = useMemo(() => buildPaletteItems(response), [response]);
  const browsing = query.trim() === "";
  const totals = useMemo(() => {
    const t: Record<Group, number> = { titles: 0, tags: 0, notes: 0 };
    for (const item of all) t[groupOf(item)] += 1;
    return t;
  }, [all]);
  // Browse mode shows 5 per group until "Show all"; purely a client-side cut of what the search already returned.
  const items = useMemo(() => {
    if (!browsing) return all;
    const seen: Record<Group, number> = { titles: 0, tags: 0, notes: 0 };
    return all.filter((item) => {
      const g = groupOf(item);
      seen[g] += 1;
      return expanded.has(g) || seen[g] <= BROWSE_LIMIT;
    });
  }, [all, browsing, expanded]);
  const input = useRef<HTMLInputElement>(null);
  const trimmed = query.trim();
  const groupHeading = (g: Group) =>
    g === "titles"
      ? browsing
        ? "Recent titles"
        : "Titles"
      : g === "tags"
        ? "Tags"
        : "Notes";

  useEffect(() => {
    const previous = document.activeElement;
    input.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  useEffect(() => setActive(0), [response]);

  useEffect(() => {
    document
      .getElementById(optionId(active))
      ?.scrollIntoView({ block: "nearest" });
  }, [active, items]);

  const activate = (item: PaletteItem | undefined) => {
    if (!item) return;
    if (item.kind === "hit") onOpenBlock(item.hit);
    else onOpenHistory(item.kind, item.suggestion.label);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) =>
        stepIndex(a, e.key === "ArrowDown" ? 1 : -1, items.length),
      );
    } else if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      activate(items[active]);
    }
  };

  let emptyText: string | null = null;
  if (!error && !loading && items.length === 0) {
    emptyText =
      trimmed === ""
        ? response?.indexing
          ? null
          : "Nothing indexed yet."
        : `No notes match “${trimmed}”.`;
  }

  return (
    <div
      className="palette__backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label="Search"
        onKeyDown={onKeyDown}
      >
        <div className="palette__field">
          <MagnifyingGlassIcon
            className="app__icon palette__icon"
            aria-hidden="true"
          />
          <input
            ref={input}
            className="palette__input"
            role="combobox"
            aria-expanded={items.length > 0}
            aria-controls={LIST_ID}
            aria-activedescendant={
              items.length > 0 ? optionId(active) : undefined
            }
            aria-label="Search notes"
            placeholder="Search notes, titles and tags"
            autoComplete="off"
            spellCheck={false}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <div
          className="palette__list"
          id={LIST_ID}
          role="listbox"
          aria-label="Results"
        >
          {error && (
            <p className="palette__message palette__message--error">{error}</p>
          )}
          {emptyText && <p className="palette__message">{emptyText}</p>}
          {items.map((item, i) => {
            const g = groupOf(item);
            const first = i === 0 || groupOf(items[i - 1]) !== g;
            const last = i === items.length - 1 || groupOf(items[i + 1]) !== g;
            const hidden =
              totals[g] - items.filter((x) => groupOf(x) === g).length;
            return (
              <div key={i} role="presentation">
                {first && (
                  <div className="palette__heading" aria-hidden="true">
                    {groupHeading(g)}
                  </div>
                )}
                <div
                  id={optionId(i)}
                  role="option"
                  aria-selected={i === active}
                  className={
                    i === active
                      ? "palette__row palette__row--active"
                      : "palette__row"
                  }
                  onMouseMove={() => i !== active && setActive(i)}
                  onClick={() => activate(item)}
                >
                  {item.kind === "hit" ? (
                    <HitRow hit={item.hit} tagColors={tagColors} />
                  ) : (
                    <div className="palette__line">
                      {item.kind === "tag" && (
                        <span
                          className="palette__dot"
                          style={{
                            background:
                              tagColorVar([item.suggestion.label], tagColors) ??
                              "var(--color-line)",
                          }}
                        />
                      )}
                      <span className="palette__title">
                        {item.kind === "tag"
                          ? `#${item.suggestion.label}`
                          : item.suggestion.label}
                      </span>
                      <span className="palette__count">
                        {item.suggestion.count}
                      </span>
                    </div>
                  )}
                </div>
                {last && browsing && hidden > 0 && (
                  <button
                    type="button"
                    className="palette__more"
                    onClick={() => setExpanded((e) => new Set(e).add(g))}
                  >
                    Show all {totals[g]}
                  </button>
                )}
              </div>
            );
          })}
        </div>
        <footer className="palette__footer">
          <span className="palette__keys">
            <kbd className="palette__kbd">↑↓</kbd> move{" "}
            <kbd className="palette__kbd">Enter</kbd> open{" "}
            <kbd className="palette__kbd">Esc</kbd> close
          </span>
          <span className="palette__status" role="status" aria-live="polite">
            {response?.indexing && (
              <span>Indexing… results may be incomplete</span>
            )}
            {response?.truncated && (
              <span>Showing the first 50 — refine your search</span>
            )}
          </span>
        </footer>
      </div>
    </div>
  );
}

function HitRow({
  hit,
  tagColors,
}: {
  hit: SearchHit;
  tagColors: Record<string, string>;
}) {
  return (
    <>
      <div className="palette__line">
        <span className="palette__title">
          {hit.title === "" ? "Untitled" : <Highlighted text={hit.titleHl} />}
        </span>
        {hit.tags.map((tag) => (
          <span key={tag} className="palette__tag">
            <span
              className="palette__dot"
              style={{
                background:
                  tagColorVar([tag], tagColors) ?? "var(--color-line)",
              }}
            />
            #{tag}
          </span>
        ))}
        <span className="palette__count">
          {fmtDateShort(hit.date)} · {fmtRange(hit.start, hit.end)}
        </span>
      </div>
      {hit.snippet.trim() !== "" && (
        <div className="palette__snippet">
          <Highlighted text={oneLine(hit.snippet)} />
        </div>
      )}
    </>
  );
}
