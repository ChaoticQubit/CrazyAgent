import type { ReactNode } from "react";

/** One section of settings. Inputs: the title, the purpose line, the actions and the rows.
 * Output: the section. */
export function SettingSection({ title, about, children }: { title: ReactNode; about?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return <section className="set-group">
    <div className="set-head"><div><h3>{title}</h3>{about && <p>{about}</p>}</div></div>
    {children}
  </section>;
}

/** One line of a settings section. Inputs: the label, the help, whether it changed, the id of
 * the control the label names, whether the row stacks and the control. Output: the row. */
export function SettingLine({ label, children }: { label: string; help?: ReactNode; changed?: boolean; htmlFor?: string; wide?: boolean; children: ReactNode }) {
  return <div className="set-row" data-setting={label}>
    <span className="set-label">{label}</span>
    <div className="set-ctl">{children}</div>
  </div>;
}
