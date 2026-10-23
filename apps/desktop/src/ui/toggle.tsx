import type { ReactNode } from "react";
import { Icon, type IconName } from "./icon";

/** An on/off switch (role "switch"). When a reason is given the switch is unavailable but
 * still focusable, and the reason describes it. Inputs: id, label, checked, change handler,
 * optional disabled reason, optional visually hidden label. */
export function Switch({ id, label, checked, onChecked, disabledReason, hideLabel = false }: {
  id: string;
  label: string;
  checked: boolean;
  onChecked: (checked: boolean) => void;
  disabledReason?: string;
  hideLabel?: boolean;
}) {
  return <span className="switch-label">
    <button id={id} type="button" role="switch" className="switch" aria-checked={checked}
      aria-disabled={disabledReason ? true : undefined} aria-describedby={disabledReason ? `${id}-reason` : undefined}
      aria-labelledby={`${id}-label`} onClick={() => { if (!disabledReason) onChecked(!checked); }} />
    <span id={`${id}-label`} className={hideLabel ? "visually-hidden" : undefined}>{label}</span>
    {disabledReason && <span id={`${id}-reason`} className="helper">{disabledReason}</span>}
  </span>;
}

/** A native checkbox with its label. Inputs: label, checked, change handler, disabled. */
export function Checkbox({ label, checked, onChecked, disabled }: {
  label: ReactNode;
  checked: boolean;
  onChecked: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return <label className="checkbox">
    <input type="checkbox" checked={checked} disabled={disabled} onChange={(event) => onChecked(event.target.checked)} />
    <span>{label}</span>
  </label>;
}

/** A radio group drawn as segments (role "radiogroup"); arrow keys move the choice.
 * Inputs: accessible label, value, options with optional icons, change handler. */
export function Segmented<T extends string>({ label, value, options, onValue }: {
  label: string;
  value: T;
  options: { value: T; label: string; icon?: IconName }[];
  onValue: (value: T) => void;
}) {
  const move = (step: number) => {
    const index = options.findIndex((option) => option.value === value);
    onValue(options[(index + step + options.length) % options.length].value);
  };
  return <div className="segmented" role="radiogroup" aria-label={label}
    onKeyDown={(event) => {
      if (event.key === "ArrowRight" || event.key === "ArrowDown") { event.preventDefault(); move(1); }
      if (event.key === "ArrowLeft" || event.key === "ArrowUp") { event.preventDefault(); move(-1); }
    }}>
    {options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={option.value === value}
      tabIndex={option.value === value ? 0 : -1} onClick={() => onValue(option.value)}>
      {option.icon && <Icon name={option.icon} size={14} />}{option.label}
    </button>)}
  </div>;
}

/** A radio choice drawn as a card with a title and up to two detail lines.
 * Inputs: group name, value, checked, change handler, title, details. */
export function RadioCard({ name, value, checked, onChecked, title, details = [] }: {
  name: string;
  value: string;
  checked: boolean;
  onChecked: (value: string) => void;
  title: ReactNode;
  details?: ReactNode[];
}) {
  return <label className="radio-card">
    <input type="radio" name={name} value={value} checked={checked} onChange={() => onChecked(value)} />
    <span className="title">{title}</span>
    {details.map((detail, index) => <span key={index} className="detail">{detail}</span>)}
  </label>;
}
