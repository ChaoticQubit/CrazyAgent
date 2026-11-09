import { createContext, useContext, useId, type ReactNode } from "react";

/** The words Find a setting keeps, lower case; blank keeps every row. */
export const SettingFind = createContext("");

/** Says whether a row stays while Find a setting holds some words. Inputs: the row's label and
 * the words. Output: the answer. */
export function settingMatches(label: string, words: string): boolean {
  return words === "" || label.toLowerCase().includes(words);
}

/** One setting row: its label with any help under it on the left and its control on the
 * right, or all stacked for a wide row; hidden while Find a setting does not match its label.
 * Inputs: the label, whether it is wide and the row's parts. Output: the row. */
export function SettingRow({ label, wide = false, children }: { label: string; wide?: boolean; children: ReactNode }) {
  const words = useContext(SettingFind);
  return <div className={wide ? "set-row wide" : "set-row"} data-setting={label} hidden={!settingMatches(label, words)}>{children}</div>;
}

/** One section of settings as the approved design draws it, named by its heading: the heading
 * and purpose line, any actions at the right, then its rows. Inputs: the title, the purpose
 * line, the actions, whether the whole section differs from what was saved and the rows.
 * Output: the section. */
export function SettingSection({ title, about, actions, changed = false, children }: {
  title: ReactNode; about?: ReactNode; actions?: ReactNode; changed?: boolean; children: ReactNode;
}) {
  const id = useId();
  return <section className="set-group" aria-labelledby={id}>
    <div className="set-head"><div className="grow"><h3 id={id}>{title}{changed && <span className="set-changed">changed</span>}</h3>{about && <p>{about}</p>}</div>{actions}</div>
    {children}
  </section>;
}

/** One line of a settings section: the label, with a "changed" mark when the value differs from
 * its default and any help under it, on the left and the control on the right. Inputs: the
 * label, the help, whether it changed, the id of the one control the label names, whether the
 * row stacks and the control. Output: the row. */
export function SettingLine({ label, help, changed = false, htmlFor, wide = false, children }: {
  label: string; help?: ReactNode; changed?: boolean; htmlFor?: string; wide?: boolean; children: ReactNode;
}) {
  const words = <>{label}{changed && <span className="set-changed">changed</span>}</>;
  return <SettingRow label={label} wide={wide}>
    {htmlFor ? <label className="set-label" htmlFor={htmlFor}>{words}</label> : <span className="set-label">{words}</span>}
    {help && <span className="set-help">{help}</span>}
    <div className="set-ctl set-line-ctl">{children}</div>
  </SettingRow>;
}
