/** The plugin tiers, lowest first; `kernel` is a kernel module's capability. */
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
