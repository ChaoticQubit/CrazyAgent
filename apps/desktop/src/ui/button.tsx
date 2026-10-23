import type { ButtonHTMLAttributes, ReactNode } from "react";
import { Icon, type IconName } from "./icon";

export type ButtonVariant = "primary" | "secondary" | "quiet" | "danger" | "danger-text";

/** A button in one of the design variants, with an optional leading icon and a pending
 * state that swaps the label and blocks clicks. Inputs: variant, size, icon, pending label,
 * native button attributes. Output: a button element (type "button" unless given). */
export function Button({ variant = "secondary", size, icon, pending = false, pendingLabel, iconOnly, children, className, disabled, type = "button", ...rest }: {
  variant?: ButtonVariant;
  size?: "small" | "large";
  icon?: IconName;
  pending?: boolean;
  pendingLabel?: string;
  iconOnly?: string;
  children?: ReactNode;
} & ButtonHTMLAttributes<HTMLButtonElement>) {
  const classes = ["btn", variant === "secondary" ? "" : variant, size ?? "", iconOnly ? "icon-only" : "", className ?? ""].filter(Boolean).join(" ");
  return <button {...rest} type={type} className={classes} disabled={disabled || pending} aria-busy={pending || undefined}
    aria-label={iconOnly ?? rest["aria-label"]} title={iconOnly ?? rest.title}>
    {pending ? <Icon name="loader-circle" spin /> : icon && <Icon name={icon} size={iconOnly ? 16 : 15} />}
    {iconOnly ? null : pending && pendingLabel ? pendingLabel : children}
  </button>;
}
