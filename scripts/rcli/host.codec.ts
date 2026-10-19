import assert from "node:assert/strict";
import { Writable } from "node:stream";
import { finished } from "node:stream/promises";
import type { MessageFrame } from "./host.messages.ts";

const FRAME_MAX = 65_536;
const QUEUE_MAX = 64;
const QUEUE_BYTES_MAX = 1_048_576;
const U64_MAX = 18_446_744_073_709_551_615n;
const utf8 = new TextEncoder();
const fatalUtf8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

type ObjectValue = Record<string, unknown>;
type ControlBody =
  | { kind: "prompt" | "steer"; turn_id: string; text: string }
  | { kind: "cancel"; turn_id: string }
  | { kind: "answer"; turn_id: string; prompt_id: string; answers: string[] }
  | { kind: "shutdown" };
export type Control = {
  protocol_version: 1;
  session_id: string;
  request_id: string;
  control: ControlBody;
};
export type EventBody =
  | MessageFrame
  | { kind: "ready" }
  | { kind: "output"; turn_id: string; text: string }
  | { kind: "input"; turn_id: string; prompt_id: string; question: string; choices: string[] }
  | { kind: "completed"; turn_id: string; status: "succeeded" | "cancelled" | "failed" };

function reject(): never {
  throw new Error("host_control_rejected");
}

function scalar(value: string): string {
  for (let index = 0; index < value.length; index++) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(++index);
      if (!(next >= 0xdc00 && next <= 0xdfff)) reject();
    } else if (unit >= 0xdc00 && unit <= 0xdfff) reject();
  }
  return value;
}

function record(value: unknown): ObjectValue {
  if (value === null || typeof value !== "object" || Array.isArray(value)) reject();
  return value as ObjectValue;
}

function exact(value: unknown, keys: string[]): ObjectValue {
  const object = record(value);
  const actual = Object.keys(object);
  if (actual.length !== keys.length || keys.some((key) => !Object.hasOwn(object, key))) reject();
  return object;
}

export function identifier(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(value) || value.includes("..")) reject();
  return value;
}

function text(value: unknown, maximum: number): string {
  if (typeof value !== "string" || value.length === 0 || utf8.encode(scalar(value)).byteLength > maximum) reject();
  return value;
}

function strictJson(source: string): unknown {
  let index = 0;
  function whitespace() {
    while (index < source.length && /[ \t\n\r]/.test(source[index]!)) index++;
  }
  function scanString(): string {
    if (source[index] !== '"') reject();
    const start = index++;
    while (index < source.length) {
      const character = source[index++];
      if (character === '"') return scalar(JSON.parse(source.slice(start, index)));
      if (character === "\\") index++;
    }
    return reject();
  }
  function scanValue(depth: number): void {
    whitespace();
    const opening = source[index];
    if (opening === "{" || opening === "[") {
      if (depth >= 8) reject();
      index++;
      whitespace();
      const closing = opening === "{" ? "}" : "]";
      if (source[index] === closing) {
        index++;
        return;
      }
      const keys = new Set<string>();
      while (index < source.length) {
        if (opening === "{") {
          whitespace();
          const key = scanString();
          if (keys.has(key)) reject();
          keys.add(key);
          whitespace();
          if (source[index++] !== ":") reject();
        }
        scanValue(depth + 1);
        whitespace();
        const separator = source[index++];
        if (separator === closing) return;
        if (separator !== ",") reject();
      }
      reject();
    } else if (opening === '"') {
      scanString();
    } else {
      const token = /^(?:null|true|false|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(source.slice(index));
      if (!token) reject();
      if (/[.eE]/.test(token[0])) reject();
      index += token[0].length;
    }
  }
  scanValue(0);
  whitespace();
  if (index !== source.length) reject();
  return JSON.parse(source);
}

export function decodeControl(payload: Uint8Array, sessionId: string): Control {
  try {
    if (!(payload instanceof Uint8Array) || payload.byteLength === 0 || payload.byteLength > FRAME_MAX) reject();
    const envelope = exact(strictJson(fatalUtf8.decode(payload)), ["protocol_version", "session_id", "request_id", "control"]);
    if (envelope.protocol_version !== 1 || identifier(envelope.session_id) !== identifier(sessionId)) reject();
    identifier(envelope.request_id);
    const body = record(envelope.control);
    switch (body.kind) {
      case "prompt":
      case "steer": {
        exact(body, ["kind", "turn_id", "text"]);
        identifier(body.turn_id);
        const value = text(body.text, 16_384);
        if (/^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]*$/.test(value)) reject();
        break;
      }
      case "cancel":
        exact(body, ["kind", "turn_id"]);
        identifier(body.turn_id);
        break;
      case "answer":
        exact(body, ["kind", "turn_id", "prompt_id", "answers"]);
        identifier(body.turn_id);
        identifier(body.prompt_id);
        if (!Array.isArray(body.answers) || body.answers.length < 1 || body.answers.length > 8) reject();
        for (const answer of body.answers) text(answer, 1024);
        break;
      case "shutdown":
        exact(body, ["kind"]);
        break;
      default:
        reject();
    }
    return envelope as Control;
  } catch {
    return reject();
  }
}

export async function* readControls(source: AsyncIterable<Uint8Array>, sessionId: string): AsyncGenerator<Control> {
  const header = new Uint8Array(4);
  let headerUsed = 0;
  let body: Uint8Array | null = null;
  let bodyUsed = 0;
  try {
    for await (const chunk of source) {
      if (!(chunk instanceof Uint8Array)) reject();
      let offset = 0;
      while (offset < chunk.byteLength) {
        if (body === null) {
          const length = Math.min(4 - headerUsed, chunk.byteLength - offset);
          header.set(chunk.subarray(offset, offset + length), headerUsed);
          headerUsed += length;
          offset += length;
          if (headerUsed !== 4) continue;
          const declared = new DataView(header.buffer).getUint32(0);
          if (declared === 0 || declared > FRAME_MAX) reject();
          body = new Uint8Array(declared);
          bodyUsed = 0;
        }
        const length = Math.min(body.byteLength - bodyUsed, chunk.byteLength - offset);
        body.set(chunk.subarray(offset, offset + length), bodyUsed);
        bodyUsed += length;
        offset += length;
        if (bodyUsed === body.byteLength) {
          const value = decodeControl(body, sessionId);
          body = null;
          headerUsed = 0;
          bodyUsed = 0;
          yield value;
        }
      }
    }
    if (headerUsed !== 0 || body !== null) reject();
  } catch {
    reject();
  }
}

function eventBody(value: unknown): EventBody {
  const body = record(value);
  switch (body.kind) {
    case "ready":
      exact(body, ["kind"]);
      return { kind: "ready" };
    case "message_started":
    case "message_reset":
    case "message_discarded":
    case "message_finished":
      exact(body, ["kind", "turn_id", "message_id"]);
      return { kind: body.kind, turn_id: identifier(body.turn_id), message_id: identifier(body.message_id) };
    case "message_delta":
      exact(body, ["kind", "turn_id", "message_id", "text"]);
      return { kind: "message_delta", turn_id: identifier(body.turn_id), message_id: identifier(body.message_id), text: text(body.text, 16_384) };
    case "output":
      exact(body, ["kind", "turn_id", "text"]);
      return { kind: "output", turn_id: identifier(body.turn_id), text: text(body.text, 16_384) };
    case "input":
      exact(body, ["kind", "turn_id", "prompt_id", "question", "choices"]);
      if (!Array.isArray(body.choices) || body.choices.length > 8) reject();
      return { kind: "input", turn_id: identifier(body.turn_id), prompt_id: identifier(body.prompt_id), question: text(body.question, 4096), choices: Array.from(body.choices, (choice) => text(choice, 1024)) };
    case "completed":
      exact(body, ["kind", "turn_id", "status"]);
      if (body.status !== "succeeded" && body.status !== "cancelled" && body.status !== "failed") reject();
      return { kind: "completed", turn_id: identifier(body.turn_id), status: body.status };
    default:
      return reject();
  }
}

export function encodeEvent(sessionId: string, instanceId: string, sequence: bigint, value: unknown): Uint8Array {
  try {
    identifier(sessionId);
    identifier(instanceId);
    if (typeof sequence !== "bigint" || sequence < 1n || sequence > U64_MAX) reject();
    const event = eventBody(value);
    const body = utf8.encode(`{"protocol_version":1,"session_id":${JSON.stringify(sessionId)},"instance_id":${JSON.stringify(instanceId)},"sequence":${sequence.toString()},"event":${JSON.stringify(event)}}`);
    if (body.byteLength === 0 || body.byteLength > FRAME_MAX) reject();
    const frame = new Uint8Array(4 + body.byteLength);
    new DataView(frame.buffer).setUint32(0, body.byteLength);
    frame.set(body, 4);
    return frame;
  } catch {
    throw new Error("host_event_rejected");
  }
}

type PendingFrame = { frame: Uint8Array; resolve: () => void; reject: (error: Error) => void };

export class BoundedEventWriter {
  private readonly destination: Writable;
  private readonly sessionId: string;
  private readonly instanceId: string;
  private readonly queue: PendingFrame[] = [];
  private queuedBytes = 0;
  private sequence = 0n;
  private failure: Error | null = null;
  private pumping: Promise<void> | null = null;
  private closing = false;
  private closeTask: Promise<void> | null = null;

  constructor(destination: Writable, sessionId: string, instanceId: string) {
    this.destination = destination;
    this.sessionId = sessionId;
    this.instanceId = instanceId;
    if (!(destination instanceof Writable)) throw new Error("host_output_failed");
    destination.on("error", () => this.fail());
    destination.on("close", () => {
      if (!this.closing) this.fail();
    });
  }

  enqueue(value: unknown): Promise<void> {
    if (this.failure) return Promise.reject(this.failure);
    if (this.closing) return Promise.reject(new Error("host_output_failed"));
    let frame: Uint8Array;
    try {
      const event = eventBody(value);
      if ((this.sequence === 0n) !== (event.kind === "ready")) throw new Error("host_event_rejected");
      frame = encodeEvent(this.sessionId, this.instanceId, this.sequence + 1n, event);
    } catch {
      return Promise.reject(new Error("host_event_rejected"));
    }
    if (this.queue.length >= QUEUE_MAX || this.queuedBytes + frame.byteLength > QUEUE_BYTES_MAX) return Promise.reject(this.fail());
    this.sequence++;
    this.queuedBytes += frame.byteLength;
    const completion = new Promise<void>((resolve, reject) => this.queue.push({ frame, resolve, reject }));
    if (!this.pumping) this.pumping = this.pump();
    return completion;
  }

  private fail(): Error {
    if (this.failure) return this.failure;
    this.failure = new Error("host_output_failed");
    for (const pending of this.queue.splice(0)) pending.reject(this.failure);
    this.queuedBytes = 0;
    this.destination.destroy();
    return this.failure;
  }

  private writeFrame(frame: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      let accepted: boolean | undefined;
      let callbackDone = false;
      let drained = false;
      const cleanup = () => {
        this.destination.off("error", onError);
        this.destination.off("close", onError);
        this.destination.off("drain", onDrain);
      };
      const onError = () => { cleanup(); reject(this.fail()); };
      const complete = () => {
        if (callbackDone && (accepted === true || drained)) { cleanup(); resolve(); }
      };
      const onDrain = () => { drained = true; complete(); };
      this.destination.once("error", onError);
      this.destination.once("close", onError);
      this.destination.once("drain", onDrain);
      try {
        accepted = this.destination.write(frame, (error) => {
          if (error) { onError(); return; }
          callbackDone = true;
          complete();
        });
        complete();
      } catch {
        onError();
      }
    });
  }

  private async pump(): Promise<void> {
    try {
      while (this.queue.length > 0) {
        const pending = this.queue[0]!;
        await this.writeFrame(pending.frame);
        if (this.failure) return;
        assert.equal(this.queue.shift(), pending);
        this.queuedBytes -= pending.frame.byteLength;
        assert(this.queuedBytes >= 0 && this.queuedBytes <= QUEUE_BYTES_MAX);
        pending.resolve();
      }
      assert.equal(this.queuedBytes, 0);
    } catch {
      this.fail();
    } finally {
      this.pumping = null;
    }
  }

  close(): Promise<void> {
    if (this.closeTask) return this.closeTask;
    this.closing = true;
    this.closeTask = (async () => {
      if (this.failure) throw this.failure;
      if (this.pumping) await this.pumping;
      if (this.failure) throw this.failure;
      try {
        const completion = finished(this.destination, { readable: false });
        this.destination.end();
        await completion;
      } catch {
        throw this.fail();
      }
    })();
    return this.closeTask;
  }
}
