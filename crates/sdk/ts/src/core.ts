/** The plugin tiers, lowest first; `kernel` is a kernel module's capability. */
export type Tier = "kernel" | "t1" | "t2" | "t3";

/** One capability's static facts, as capgen writes them: name, tier and permission. */
export type Capability = { readonly name: string; readonly tier: Tier; readonly needs: string };

/** Why the bus did not complete a call, mirroring `sdk::Refusal`; codes pass unchanged. */
export type Refusal =
  | { readonly kind: "not_granted"; readonly capability: string; readonly needs: string }
  | { readonly kind: "tier_too_high"; readonly capability: string }
  | { readonly kind: "plugin_off"; readonly capability: string }
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

/** One delivered event record, as the kernel sends it after commit. */
export type EventRecord = {
  readonly seq: string;
  readonly revision: string;
  readonly aggregate: string;
  readonly kind: string;
  readonly schema: number;
  readonly body: Wire;
};

/** Stops one subscription. Calling it twice is a no-op. */
export type Unsubscribe = () => void;

/** The transport the kernel bound to this plugin. `call` takes the capability name and the
 * input keyed by parameter name and gives the reply; `subscribe` takes the kinds, the last
 * seq seen as a decimal (or null) and the record callback and gives the stop. The plugin
 * never names itself; the kernel decides which kinds it may see. */
export interface Host {
  call(capability: string, input: { readonly [key: string]: Wire }): Promise<WireReply>;
  subscribe(
    kinds: readonly string[],
    after: string | null,
    onRecord: (record: EventRecord) => void,
  ): Unsubscribe;
}

/** The most bytes one `text` value of a capability call may hold. */
export const TEXT_BYTES_MAX = 8_192;
/** The most bytes one `bytes` value of a capability call may hold. */
export const BYTES_MAX = 4_194_304;
/** The most items one `list` value of a capability call may hold. */
export const LIST_ITEMS_MAX = 64;

const U64_LIMIT = 1n << 64n;
const DECIMAL = /^(0|[1-9][0-9]{0,19})$/;
const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** Sends one call and decodes its reply. Inputs: the bound host, the capability, the input
 * keyed by parameter name and the output decoder. Output: the value, the bus refusal, or
 * `failed` with `sdk_transport_failed` when the transport itself throws. */
export async function call<T>(
  host: Host,
  capability: Capability,
  input: { readonly [key: string]: Wire },
  decode: (value: Wire) => T,
): Promise<Result<T>> {
  let reply: WireReply;
  try {
    reply = await host.call(capability.name, input);
  } catch {
    return { ok: false, refusal: { kind: "failed", code: "sdk_transport_failed" } };
  }
  if ("refused" in reply) {
    return { ok: false, refusal: reply.refused };
  }
  return { ok: true, value: decode(reply.ok) };
}

/** Subscribes to some kinds of one stem and delivers each event once, in seq order.
 * Inputs: the host, the stem's kinds and decoder, the last seq seen (or null), the handler.
 * Output: the unsubscribe. A seq at or below the last one seen is skipped (at-least-once
 * delivery); a body that does not decode reaches the handler as `sdk_event_undecodable`. */
export function subscribe<E>(
  host: Host,
  kinds: readonly string[],
  decode: (kind: string, body: Wire) => E | null,
  after: bigint | null,
  handler: (event: Result<{ readonly seq: bigint; readonly event: E }>) => void,
): Unsubscribe {
  if (kinds.length === 0 || kinds.length > LIST_ITEMS_MAX) {
    throw new RangeError("sdk_subscribe_kinds");
  }
  const start = after === null ? null : encodeU64(after);
  const wanted = new Set(kinds);
  const undecodable = () =>
    handler({ ok: false, refusal: { kind: "failed", code: "sdk_event_undecodable" } });
  let last = after;
  let open = true;
  const onRecord = (record: EventRecord): void => {
    if (!open) {
      return;
    }
    let seq: bigint;
    try {
      seq = decodeU64(record.seq);
    } catch {
      undecodable();
      return;
    }
    if (last !== null && seq <= last) {
      return;
    }
    last = seq;
    if (!wanted.has(record.kind)) {
      return;
    }
    let event: E | null;
    try {
      event = decode(record.kind, record.body);
    } catch {
      undecodable();
      return;
    }
    if (event !== null) {
      handler({ ok: true, value: { seq, event } });
    }
  };
  let stop: Unsubscribe;
  try {
    stop = host.subscribe(kinds, start, onRecord);
  } catch {
    open = false;
    handler({ ok: false, refusal: { kind: "failed", code: "sdk_transport_failed" } });
    return () => {};
  }
  return () => {
    if (open) {
      open = false;
      stop();
    }
  };
}

/** Writes a u64 as its decimal string. Input: a bigint in [0, 2^64). Output: the string. */
export function encodeU64(value: bigint): string {
  if (value < 0n || value >= U64_LIMIT) {
    throw new RangeError("sdk_u64_out_of_range");
  }
  return value.toString(10);
}

/** Reads a u64 the kernel sent. Input: the wire value. Output: the bigint; anything but a
 * canonical decimal string below 2^64 breaks the wire contract and throws. */
export function decodeU64(value: Wire): bigint {
  if (typeof value !== "string" || !DECIMAL.test(value)) {
    throw new TypeError("sdk_wire_u64");
  }
  const parsed = BigInt(value);
  if (parsed >= U64_LIMIT) {
    throw new TypeError("sdk_wire_u64");
  }
  return parsed;
}

/** Writes bytes as standard padded base64. Input: the bytes. Output: the string. */
export function encodeBytes(value: Uint8Array): string {
  let text = "";
  for (let index = 0; index < value.length; index += 3) {
    const a = value[index]!;
    const b = value[index + 1];
    const c = value[index + 2];
    text += ALPHABET[a >> 2]!;
    text += ALPHABET[((a & 3) << 4) | ((b ?? 0) >> 4)]!;
    text += b === undefined ? "=" : ALPHABET[((b & 15) << 2) | ((c ?? 0) >> 6)]!;
    text += c === undefined ? "=" : ALPHABET[c & 63]!;
  }
  return text;
}

/** Reads bytes the kernel sent. Input: the wire value. Output: the bytes; anything but
 * canonical padded base64 breaks the wire contract and throws. */
export function decodeBytes(value: Wire): Uint8Array {
  if (typeof value !== "string" || value.length % 4 !== 0) {
    throw new TypeError("sdk_wire_bytes");
  }
  const pad = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
  const out = new Uint8Array((value.length / 4) * 3 - pad);
  let at = 0;
  for (let index = 0; index < value.length; index += 4) {
    const quad = [0, 1, 2, 3].map((offset) => {
      const char = value[index + offset]!;
      return char === "=" && index + offset >= value.length - pad ? 0 : ALPHABET.indexOf(char);
    });
    if (quad.some((sextet) => sextet < 0)) {
      throw new TypeError("sdk_wire_bytes");
    }
    const triple = (quad[0]! << 18) | (quad[1]! << 12) | (quad[2]! << 6) | quad[3]!;
    for (const shift of [16, 8, 0]) {
      if (at < out.length) {
        out[at++] = (triple >> shift) & 255;
      }
    }
  }
  if (encodeBytes(out) !== value) {
    throw new TypeError("sdk_wire_bytes");
  }
  return out;
}
