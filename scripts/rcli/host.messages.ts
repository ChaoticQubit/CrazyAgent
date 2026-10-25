import assert from "node:assert/strict";
import type { AgentEvent } from "../../.local-state/tools/rcli-source/src/core/bus/events.ts";
import { FRAME_TEXT_MAX } from "./host.codec.ts";

export const MESSAGE_JSON_MAX = 458_752;

export type MessageFrame =
  | { kind: "message_started" | "message_reset" | "message_discarded" | "message_finished"; turn_id: string; message_id: string }
  | { kind: "message_delta"; turn_id: string; message_id: string; text: string };

type MessageState =
  | { hostId: string; phase: "streaming" | "finished" }
  | { hostId: string | null; phase: "discarded" }
  | { hostId: string | null; phase: "overflowed" };

function reject(): never {
  throw new Error("host_sdk_event_rejected");
}

function record(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) reject();
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) reject();
  const keys = Reflect.ownKeys(value);
  if (keys.length > 32 || keys.some((key) => typeof key !== "string")) reject();
  for (const key of keys) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor || !descriptor.enumerable || !("value" in descriptor)) reject();
  }
  return value as Record<string, unknown>;
}

function textChunks(value: unknown, maximumBytes: number, maximumJson: number): string[] | null {
  if (typeof value !== "string") reject();
  const chunks: string[] = [];
  let start = 0;
  let chunkBytes = 0;
  let chunkJson = 2;
  let totalBytes = 0;
  let totalJson = 2;
  for (let index = 0; index < value.length;) {
    const unit = value.charCodeAt(index);
    let units = 1;
    let bytes = unit <= 0x7f ? 1 : unit <= 0x7ff ? 2 : 3;
    let json = unit >= 0x20 ? unit === 0x22 || unit === 0x5c ? 2 : bytes : unit >= 0x08 && unit <= 0x0d && unit !== 0x0b ? 2 : 6;
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) reject();
      units = 2;
      bytes = 4;
      json = 4;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      reject();
    }
    if (totalJson + json > maximumJson) return null;
    if (totalBytes + bytes > maximumBytes) reject();
    if (chunkBytes + bytes > 16384 || chunkJson + json > FRAME_TEXT_MAX) {
      chunks.push(value.slice(start, index));
      start = index;
      chunkBytes = 0;
      chunkJson = 2;
    }
    chunkBytes += bytes;
    chunkJson += json;
    totalBytes += bytes;
    totalJson += json;
    index += units;
  }
  if (start < value.length) chunks.push(value.slice(start));
  return chunks;
}

function opaqueId(value: unknown): string {
  if (typeof value !== "string" || value.length === 0 || value.length > 256) reject();
  textChunks(value, 256, Number.POSITIVE_INFINITY);
  return value;
}

function stateInvariant(value: MessageState | undefined): asserts value is MessageState {
  assert(value !== null && value !== undefined &&
    ["streaming", "finished", "discarded", "overflowed"].includes(value.phase) &&
    (value.hostId === null ? value.phase === "discarded" || value.phase === "overflowed" :
      typeof value.hostId === "string" && /^msg-[1-9][0-9]{0,2}$/.test(value.hostId) &&
      Number(value.hostId.slice(4)) <= 128), "host_message_identity_invariant");
}

export class SdkMessageProjection {
  private readonly turnId: string;
  private readonly sdkTurnId: string;
  private readonly messages = new Map<string, MessageState>();
  private nextMessage = 1;
  private closed = false;
  private overflow = false;

  constructor(turnId: string, sdkTurnId: string) {
    if (typeof turnId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(turnId) || turnId.includes("..")) reject();
    this.turnId = turnId;
    this.sdkTurnId = opaqueId(sdkTurnId);
  }

  accept(event: AgentEvent): MessageFrame[] {
    if (this.closed) reject();
    const input = record(event);
    if (typeof input.type !== "string") reject();
    if (!["assistant:delta", "assistant:message", "assistant:message:discard"].includes(input.type)) return [];
    const discard = input.type === "assistant:message:discard";
    const expected = discard ? ["messageId", "turnId", "type"] : ["messageId", "text", "turnId", "type"];
    if (Object.keys(input).sort().join(",") !== expected.join(",")) reject();
    if (opaqueId(input.turnId) !== this.sdkTurnId) reject();
    const nativeId = opaqueId(input.messageId);
    const previous = this.messages.get(nativeId);
    if (this.messages.has(nativeId)) stateInvariant(previous);
    if (previous?.phase === "overflowed") return [];
    if (discard) {
      if (previous?.phase === "discarded") reject();
      if (!previous) {
        this.admit();
        this.messages.set(nativeId, { hostId: null, phase: "discarded" });
        return [];
      }
      const hostId = previous.hostId;
      this.messages.set(nativeId, { hostId, phase: "discarded" });
      return [{ kind: "message_discarded", turn_id: this.turnId, message_id: hostId }];
    }
    const chunks = textChunks(input.text, MESSAGE_JSON_MAX, MESSAGE_JSON_MAX);
    if (previous && previous.phase !== "streaming") reject();
    if (chunks === null) {
      if (!previous) this.admit();
      this.overflow = true;
      this.messages.set(nativeId, { hostId: previous?.hostId ?? null, phase: "overflowed" });
      return previous ? [{ kind: "message_discarded", turn_id: this.turnId, message_id: previous.hostId }] : [];
    }
    if (input.type === "assistant:delta" && chunks.length === 0) return [];
    const output: MessageFrame[] = [];
    let current = previous;
    if (!current) {
      this.admit();
      const sequence = this.nextMessage;
      this.nextMessage += 1;
      assert(Number.isSafeInteger(sequence) && sequence >= 1 && sequence <= 128,
        "host_message_counter_invariant");
      const hostId = `msg-${sequence}`;
      current = { hostId, phase: "streaming" };
      this.messages.set(nativeId, current);
      output.push({ kind: "message_started", turn_id: this.turnId, message_id: hostId });
    } else if (input.type === "assistant:message") {
      output.push({ kind: "message_reset", turn_id: this.turnId, message_id: current.hostId });
    }
    const hostId = current.hostId;
    for (const text of chunks) {
      output.push({ kind: "message_delta", turn_id: this.turnId, message_id: hostId, text });
    }
    if (input.type === "assistant:message") {
      current.phase = "finished";
      output.push({ kind: "message_finished", turn_id: this.turnId, message_id: hostId });
    }
    return output;
  }

  get overflowed(): boolean {
    return this.overflow;
  }

  close(): void {
    this.closed = true;
    this.messages.clear();
  }

  private admit(): void {
    if (this.messages.size >= 128) reject();
  }
}
