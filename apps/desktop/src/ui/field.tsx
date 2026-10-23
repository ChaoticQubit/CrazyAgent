import { useId, useState, type InputHTMLAttributes, type ReactNode, type RefObject } from "react";
import { Icon } from "./icon";

function describedBy(id: string, helper?: ReactNode, error?: string): string | undefined {
  return [helper ? `${id}-helper` : "", error ? `${id}-error` : ""].filter(Boolean).join(" ") || undefined;
}

function Messages({ id, helper, error }: { id: string; helper?: ReactNode; error?: string }) {
  return <>
    {helper && <p className="helper" id={`${id}-helper`}>{helper}</p>}
    {error && <p className="field-error" id={`${id}-error`} role="alert">{error}</p>}
  </>;
}

function Counter({ value, maxLength, counterAt }: { value: string; maxLength?: number; counterAt?: number }) {
  const length = [...value].length;
  if (!maxLength || counterAt === undefined || length < counterAt) return null;
  return <span className="counter-text" aria-live="polite">{length.toLocaleString("en-US")} of {maxLength.toLocaleString("en-US")} characters</span>;
}

/** A labelled one-line text input with helper text, an error linked by aria-describedby and
 * an optional character counter shown near the limit. Inputs: id, label, value, change
 * handler, helper, error, max length, counter threshold, native input attributes. */
export function TextField({ id, label, value, onValue, helper, error, maxLength, counterAt, ...rest }: {
  id: string;
  label: string;
  value: string;
  onValue: (value: string) => void;
  helper?: ReactNode;
  error?: string;
  maxLength?: number;
  counterAt?: number;
} & Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "id">) {
  return <div className="field">
    <label htmlFor={id}>{label}</label>
    <input {...rest} id={id} className="input" value={value} maxLength={maxLength} aria-invalid={error ? true : undefined}
      aria-describedby={describedBy(id, helper, error)} onChange={(event) => onValue(event.target.value)} />
    <Counter value={value} maxLength={maxLength} counterAt={counterAt} />
    <Messages id={id} helper={helper} error={error} />
  </div>;
}

/** A labelled multi-line text area with helper, error and an optional near-limit counter.
 * Inputs: id, label, value, change handler, rows, helper, error, max length, counter threshold. */
export function TextArea({ id, label, value, onValue, rows = 3, helper, error, maxLength, counterAt }: {
  id: string;
  label: string;
  value: string;
  onValue: (value: string) => void;
  rows?: number;
  helper?: ReactNode;
  error?: string;
  maxLength?: number;
  counterAt?: number;
}) {
  return <div className="field">
    <label htmlFor={id}>{label}</label>
    <textarea id={id} className="textarea" rows={rows} value={value} maxLength={maxLength} aria-invalid={error ? true : undefined}
      aria-describedby={describedBy(id, helper, error)} onChange={(event) => onValue(event.target.value)} />
    <Counter value={value} maxLength={maxLength} counterAt={counterAt} />
    <Messages id={id} helper={helper} error={error} />
  </div>;
}

/** A masked, uncontrolled secret input with no reveal and no autocomplete. React never holds
 * the value; the caller reads it through the ref once, sends it and clears the field.
 * Inputs: id, label, ref, helper, error, max length. */
export function SecretField({ id, label, inputRef, helper, error, maxLength = 8192 }: {
  id: string;
  label: string;
  inputRef: RefObject<HTMLInputElement | null>;
  helper?: ReactNode;
  error?: string;
  maxLength?: number;
}) {
  return <div className="field">
    <label htmlFor={id}>{label}</label>
    <input id={id} ref={inputRef} className="input" type="password" autoComplete="off" spellCheck={false} maxLength={maxLength}
      aria-invalid={error ? true : undefined} aria-describedby={describedBy(id, helper, error)} />
    <Messages id={id} helper={helper} error={error} />
  </div>;
}

export interface SelectOption {
  value: string;
  label: string;
  disabledReason?: string;
}

/** A labelled native select; an option with a disabled reason is listed but unavailable and
 * shows the reason in its text. Inputs: id, label, value, options, change handler, helper. */
export function Select({ id, label, value, options, onValue, helper, error, hideLabel = false }: {
  id: string;
  label: string;
  value: string;
  options: SelectOption[];
  onValue: (value: string) => void;
  helper?: ReactNode;
  error?: string;
  hideLabel?: boolean;
}) {
  return <div className="field">
    <label htmlFor={id} className={hideLabel ? "visually-hidden" : undefined}>{label}</label>
    <select id={id} className="select" value={value} aria-describedby={describedBy(id, helper, error)} onChange={(event) => onValue(event.target.value)}>
      {options.map((option) => <option key={option.value} value={option.value} disabled={Boolean(option.disabledReason)}>
        {option.disabledReason ? `${option.label} - ${option.disabledReason}` : option.label}
      </option>)}
    </select>
    <Messages id={id} helper={helper} error={error} />
  </div>;
}

/** A search input with a leading icon and a visually hidden label.
 * Inputs: label, value, change handler. Output: the search field. */
export function SearchField({ label, value, onValue }: { label: string; value: string; onValue: (value: string) => void }) {
  const id = useId();
  return <span className="search">
    <label htmlFor={id} className="visually-hidden">{label}</label>
    <Icon name="search" size={14} />
    <input id={id} className="input" type="search" placeholder={label} value={value} maxLength={200} onChange={(event) => onValue(event.target.value)} />
  </span>;
}

/** Adds a chip value: trimmed, case-insensitively unique, bounded in count and length.
 * Inputs: current values, the new text, max count and max characters. Output: the new list
 * or a plain error. */
export function addChip(values: string[], text: string, max: number, maxLength: number): { values: string[] } | { error: string } {
  const value = text.trim().replace(/\s+/g, " ");
  if (!value) return { values };
  if ([...value].length > maxLength) return { error: `Keep each one under ${maxLength + 1} characters.` };
  if (/[\u0000-\u001f\u007f-\u009f]/u.test(value)) return { error: "Use one line without special characters." };
  if (values.some((existing) => existing.toLowerCase() === value.toLowerCase())) return { values };
  if (values.length >= max) return { error: `You can add up to ${max}.` };
  return { values: [...values, value] };
}

/** Chips for a short list (tech stack): removable chips, a text entry that adds on Enter or
 * comma, and catalog suggestions shown as quick-add chips. Inputs: id, label, values,
 * change handler, suggestions, max count and length, helper. */
export function ChipInput({ id, label, values, onValues, suggestions = [], max = 12, maxLength = 40, helper }: {
  id: string;
  label: string;
  values: string[];
  onValues: (values: string[]) => void;
  suggestions?: string[];
  max?: number;
  maxLength?: number;
  helper?: ReactNode;
}) {
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [focused, setFocused] = useState(false);
  const commit = (value: string) => {
    const next = addChip(values, value, max, maxLength);
    if ("error" in next) { setError(next.error); return; }
    setError("");
    setText("");
    onValues(next.values);
  };
  const open = suggestions.filter((item) => !values.some((value) => value.toLowerCase() === item.toLowerCase()) &&
    item.toLowerCase().includes(text.trim().toLowerCase())).slice(0, 12);
  return <div className="field chip-input">
    <label htmlFor={id}>{label}</label>
    {values.length > 0 && <ul className="chips plain-list" aria-label={`${label} chosen`}>
      {values.map((value) => <li key={value} className="chip">{value}
        <button type="button" aria-label={`Remove ${value}`} onClick={() => onValues(values.filter((item) => item !== value))}><Icon name="x" size={12} /></button>
      </li>)}
    </ul>}
    <input id={id} className="input" value={text} maxLength={maxLength + 1} aria-describedby={describedBy(id, helper, error)}
      onFocus={() => setFocused(true)} onBlur={() => { if (text.trim()) commit(text); }}
      onChange={(event) => { setText(event.target.value); setError(""); }}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === ",") { event.preventDefault(); commit(text); }
        if (event.key === "Backspace" && !text && values.length) onValues(values.slice(0, -1));
      }} />
    {(focused || text) && open.length > 0 && <div className="chips" aria-label={`${label} suggestions`}>
      {open.map((item) => <button type="button" key={item} className="chip chip-option" onMouseDown={(event) => event.preventDefault()} onClick={() => commit(item)}>
        <Icon name="plus" size={12} />{item}
      </button>)}
    </div>}
    <Messages id={id} helper={helper} error={error} />
  </div>;
}
