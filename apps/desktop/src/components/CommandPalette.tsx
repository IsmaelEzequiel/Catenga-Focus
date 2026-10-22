import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { HistoryKind, SearchHit } from "../api";
import { useSearch } from "../hooks/useSearch";
import { buildPaletteItems, fmtRange, stepIndex, type PaletteItem } from "../lib/search";
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
const isHistory = (item: PaletteItem) => item.kind !== "hit";

export function CommandPalette({ tagColors, onClose, onOpenBlock, onOpenHistory }: Props) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const { response, loading, error } = useSearch(query);
  const items = useMemo(() => buildPaletteItems(response), [response]);
  const input = useRef<HTMLInputElement>(null);
  const trimmed = query.trim();

  useEffect(() => {
    const previous = document.activeElement;
    input.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  useEffect(() => setActive(0), [response]);

  useEffect(() => {
    document.getElementById(optionId(active))?.scrollIntoView({ block: "nearest" });
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
      setActive((a) => stepIndex(a, e.key === "ArrowDown" ? 1 : -1, items.length));
    } else if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      activate(items[active]);
    }
  };

  let emptyText: string | null = null;
  if (!error && !loading && items.length === 0) {
    emptyText = trimmed === "" ? (response?.indexing ? null : "Nothing indexed yet.") : `No notes match “${trimmed}”.`;
  }

  return (
    <div
      className="palette__backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="palette" role="dialog" aria-modal="true" aria-label="Search" onKeyDown={onKeyDown}>
        <input
          ref={input}
          className="palette__input"
          role="combobox"
          aria-expanded={items.length > 0}
          aria-controls={LIST_ID}
          aria-activedescendant={items.length > 0 ? optionId(active) : undefined}
          aria-label="Search notes"
          placeholder="Search notes, titles and tags"
          autoComplete="off"
          spellCheck={false}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="palette__list" id={LIST_ID} role="listbox" aria-label="Results">
          {error && <p className="palette__message palette__message--error">{error}</p>}
          {emptyText && <p className="palette__message">{emptyText}</p>}
          {items.map((item, i) => {
            const prev = items[i - 1];
            const heading =
              i === 0 || isHistory(item) !== isHistory(prev)
                ? item.kind === "hit"
                  ? "Notes"
                  : trimmed === ""
                    ? "Browse"
                    : "History"
                : null;
            return (
              <div key={i} role="presentation">
                {heading && (
                  <div className="palette__heading" aria-hidden="true">
                    {heading}
                  </div>
                )}
                <div
                  id={optionId(i)}
                  role="option"
                  aria-selected={i === active}
                  className={i === active ? "palette__row palette__row--active" : "palette__row"}
                  onMouseMove={() => i !== active && setActive(i)}
                  onClick={() => activate(item)}
                >
                  {item.kind === "hit" ? (
                    <HitRow hit={item.hit} tagColors={tagColors} />
                  ) : (
                    <div className="palette__line">
                      <span className="palette__badge">{item.kind === "title" ? "Title" : "Tag"}</span>
                      <span className="palette__title">
                        History · {item.kind === "tag" ? `#${item.suggestion.label}` : item.suggestion.label}
                      </span>
                      <span className="palette__meta">{item.suggestion.count}</span>
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
        <footer className="palette__footer" role="status" aria-live="polite">
          {response?.indexing && <span>Indexing… results may be incomplete</span>}
          {response?.truncated && <span>Showing the first 50 — refine your search</span>}
        </footer>
      </div>
    </div>
  );
}

function HitRow({ hit, tagColors }: { hit: SearchHit; tagColors: Record<string, string> }) {
  return (
    <>
      <div className="palette__line">
        <span className="palette__meta">{fmtDateShort(hit.date)}</span>
        <span className="palette__meta">{fmtRange(hit.start, hit.end)}</span>
        <span className="palette__title">{hit.title === "" ? "Untitled" : <Highlighted text={hit.titleHl} />}</span>
        {hit.tags.map((tag) => (
          <span key={tag} className="palette__tag">
            <span className="palette__dot" style={{ background: tagColorVar([tag], tagColors) ?? "var(--color-line)" }} />#{tag}
          </span>
        ))}
      </div>
      {hit.snippet.trim() !== "" && (
        <div className="palette__snippet">
          <Highlighted text={hit.snippet} />
        </div>
      )}
    </>
  );
}
