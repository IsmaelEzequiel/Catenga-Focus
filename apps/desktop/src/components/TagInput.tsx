import { useState, type KeyboardEvent } from "react";
import { XMarkIcon } from "@heroicons/react/20/solid";
import type { Tag } from "../api";
import { addTagChips, suggestTags, tagVar } from "../lib/tags";
import "../projects.css";

interface Props {
  value: string[];
  onChange: (next: string[]) => void;
  /** Known tags, for suggestions and chip colors. */
  known: Tag[];
  label: string;
  placeholder?: string;
}

/** Chip input: pick an existing tag, or type a new one and press Enter / comma / space. Backspace removes the last. */
export function TagInput({ value, onChange, known, label, placeholder }: Props) {
  const [text, setText] = useState("");
  const [focused, setFocused] = useState(false);
  const colorOf = (name: string) => {
    const hit = known.find((t) => t.name.toLowerCase() === name.toLowerCase());
    return tagVar(hit?.color ?? "sun");
  };
  const suggestions = focused
    ? suggestTags(
        known.map((t) => t.name),
        value,
        text,
      )
    : [];

  const commit = (raw: string) => {
    const next = addTagChips(value, raw);
    if (next.length !== value.length) onChange(next);
    setText("");
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing) return;
    if ((e.key === "Enter" || e.key === "," || e.key === " ") && text.trim() !== "") {
      e.preventDefault();
      commit(text);
    } else if (e.key === "Backspace" && text === "" && value.length > 0) {
      onChange(value.slice(0, -1));
    }
  };

  return (
    <div className="taginput">
      <div className="taginput__box">
        {value.map((name) => (
          <span key={name.toLowerCase()} className="taginput__chip" style={{ ["--chip" as string]: colorOf(name) }}>
            #{name}
            <button
              type="button"
              className="taginput__remove"
              aria-label={`Remove tag ${name}`}
              onClick={() => onChange(value.filter((n) => n !== name))}
            >
              <XMarkIcon aria-hidden="true" />
            </button>
          </span>
        ))}
        <input
          className="taginput__input"
          aria-label={label}
          value={text}
          placeholder={value.length === 0 ? placeholder : undefined}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKeyDown}
          onFocus={() => setFocused(true)}
          onBlur={() => {
            setFocused(false);
            if (text.trim() !== "") commit(text);
          }}
        />
      </div>
      {suggestions.length > 0 && (
        <ul className="taginput__list" role="listbox" aria-label="Existing tags">
          {suggestions.map((name) => (
            <li key={name}>
              <button
                type="button"
                role="option"
                aria-selected="false"
                className="taginput__option"
                style={{ ["--chip" as string]: colorOf(name) }}
                // mousedown, so the pick lands before the input's blur commits the half-typed text
                onMouseDown={(e) => {
                  e.preventDefault();
                  onChange(addTagChips(value, name));
                  setText("");
                }}
              >
                #{name}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
