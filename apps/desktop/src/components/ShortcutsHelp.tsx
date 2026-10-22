import { useEffect, useRef, type KeyboardEvent } from "react";
import { shortcutRows } from "../lib/keys";
import "../keyhelp.css";

const IS_MAC = navigator.userAgent.includes("Mac");

/** `?` overlay: every shortcut, in two columns. Esc closes. */
export function ShortcutsHelp({ onClose }: { onClose: () => void }) {
  const panel = useRef<HTMLDivElement>(null);
  const rows = shortcutRows(IS_MAC);
  const half = Math.ceil(rows.length / 2);

  useEffect(() => {
    const previous = document.activeElement;
    panel.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  };

  return (
    <div
      className="keyhelp__backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div ref={panel} className="keyhelp" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts" tabIndex={-1} onKeyDown={onKeyDown}>
        <h2 className="keyhelp__title">Keyboard shortcuts</h2>
        <div className="keyhelp__cols">
          {[rows.slice(0, half), rows.slice(half)].map((col, i) => (
            <dl key={i} className="keyhelp__list">
              {col.map((r) => (
                <div key={r.label} className="keyhelp__row">
                  <dt className="keyhelp__keys">
                    {r.keys.map((k) => (
                      <kbd key={k} className="keyhelp__kbd">
                        {k}
                      </kbd>
                    ))}
                  </dt>
                  <dd className="keyhelp__label">{r.label}</dd>
                </div>
              ))}
            </dl>
          ))}
        </div>
        <button className="app__btn" onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}
