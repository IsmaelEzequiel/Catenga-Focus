import { useEffect, useRef, type KeyboardEvent } from "react";
import { shortcutGroups } from "../lib/keys";
import "../keyhelp.css";

const IS_MAC = navigator.userAgent.includes("Mac");

/** `?` overlay: every shortcut, in two columns. Esc closes. */
export function ShortcutsHelp({ onClose }: { onClose: () => void }) {
  const panel = useRef<HTMLDivElement>(null);
  const groups = shortcutGroups(IS_MAC);

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
      <div
        ref={panel}
        className="keyhelp"
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard shortcuts"
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <header className="keyhelp__head">
          <h2 className="keyhelp__title">Keyboard shortcuts</h2>
          <button
            type="button"
            className="keyhelp__close"
            onClick={onClose}
            aria-label="Close"
          >
            Esc
          </button>
        </header>
        <div className="keyhelp__cols">
          {groups.map((g) => (
            <section
              key={g.name}
              className="keyhelp__group"
              aria-label={g.name}
            >
              <h3 className="keyhelp__group-name">{g.name}</h3>
              <dl className="keyhelp__list">
                {g.rows.map((r) => (
                  <div key={r.label} className="keyhelp__row">
                    <dt className="keyhelp__label">{r.label}</dt>
                    <dd className="keyhelp__keys">
                      {r.keys.map((k) => (
                        <kbd key={k} className="keyhelp__kbd">
                          {k}
                        </kbd>
                      ))}
                    </dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
