/** The plugin tiers of DEC-206 Q5, lowest first; `kernel` is a kernel module's capability. */
export type Tier = "kernel" | "t1" | "t2" | "t3";

/** One capability's static facts, as capgen writes them: name, tier and permission. */
export type Capability = { readonly name: string; readonly tier: Tier; readonly needs: string };

/** Why the bus did not complete a call, mirroring `sdk::Refusal`; codes pass unchanged. */
export type Refusal =
  | { readonly kind: "not_granted"; readonly capability: string; readonly needs: string }
  | { readonly kind: "tier_too_high"; readonly capability: string }
  | { readonly kind: "rejected"; readonly code: string }
  | { readonly kind: "failed"; readonly code: string };

/** A call's outcome: its value, or the refusal. A refusal is a value, never thrown. */
export type Result<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly refusal: Refusal };

/** One JSON value as a call carries it across the bus. */
export type Wire =
  | null
  | boolean
  | number
  | string
  | readonly Wire[]
  | { readonly [key: string]: Wire };

/** The bus's reply to one call: the output, or the refusal. */
export type WireReply = { readonly ok: Wire } | { readonly refused: Refusal };

/** The transport the kernel bound to this plugin. Input: the capability name and the input
 * keyed by parameter name. Output: the reply. The plugin never names itself in a call. */
export interface Host {
  call(capability: string, input: { readonly [key: string]: Wire }): Promise<WireReply>;
}

/** The most bytes one `text` value of a capability call may hold. */
export const TEXT_BYTES_MAX = 8_192;
/** The most bytes one `bytes` value of a capability call may hold. */
export const BYTES_MAX = 4_194_304;
/** The most items one `list` value of a capability call may hold. */
export const LIST_ITEMS_MAX = 64;
