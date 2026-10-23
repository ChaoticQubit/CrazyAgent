import assert from "node:assert/strict";
import type {
  AgentEvent, UsageInfo,
} from "../../.local-state/tools/rcli-source/src/core/bus/events.ts";

type ToolObservation = Extract<AgentEvent, { type:
  | "tool:requested" | "tool:pending" | "tool:start" | "tool:retracted"
  | "tool:result" | "tool:error" | "agent:child_tool_start" | "agent:child_tool_result"
}>;
export type ObservationFrame =
  | { kind: "tool_observation"; scope: "explicit_sdk_turn" | "turn_unattributed";
      event: ToolObservation }
  | { kind: "usage_observation"; scope: "turn_unattributed"; provenance: "sdk_reported";
      accounting: "UNKNOWN"; usage: UsageInfo }
  | { kind: "observation_gap"; scope: "turn_unattributed"; source_type: string;
      reason: "unsupported_event" | "unqualified_progress"; tool_call_id?: string };
type DataRecord = Record<string, unknown>;
type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
type CopyBudget = { nodes: number; bytes: number };
const TOOL_FIELDS: Record<string, [string[], string[]]> = {
  "tool:requested": [["turnId", "calls"], []],
  "tool:pending": [["toolCallId", "name", "inputSummary"], []],
  "tool:start": [["toolCallId", "name", "inputSummary"], ["agentMode"]],
  "tool:retracted": [["toolCallId"], []],
  "tool:result": [["toolCallId", "name", "title"], ["detail", "detailLines", "agentOutput"]],
  "tool:error": [["toolCallId", "name", "error"], []],
  "agent:child_tool_start": [["agentToolCallId", "call"], []],
  "agent:child_tool_result": [["agentToolCallId", "childToolCallId", "status"], []],
};
const USAGE_FIELDS = ["inputTokens", "outputTokens", "totalTokens", "cachedTokens",
  "cacheWriteTokens", "costUsd", "contextTokens", "contextLimit", "model", "provider"];

function reject(): never { throw new Error("host_observation_rejected"); }

export function observationRecord(value: unknown): DataRecord {
  if (value === null || typeof value !== "object" || Array.isArray(value)) reject();
  if (![Object.prototype, null].includes(Object.getPrototypeOf(value))) reject();
  const keys = Reflect.ownKeys(value);
  if (keys.length > 32) reject();
  for (const key of keys) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (typeof key !== "string" || !descriptor?.enumerable || !("value" in descriptor)) reject();
  }
  return value as DataRecord;
}

function shape(value: unknown, required: string[], optional: string[] = []): DataRecord {
  const body = observationRecord(value);
  if (required.some((key) => !Object.hasOwn(body, key)) ||
      Object.keys(body).some((key) => !required.includes(key) && !optional.includes(key))) reject();
  return body;
}

function text(value: unknown, maximum: number, empty = false): string {
  if (typeof value !== "string" || (!empty && value.length === 0) ||
      value.length > maximum || !value.isWellFormed() ||
      Buffer.byteLength(value, "utf8") > maximum) reject();
  return value;
}

function number(value: unknown, integer = true): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 ||
      value > Number.MAX_SAFE_INTEGER || (integer && !Number.isSafeInteger(value))) reject();
  return value;
}

function charge(budget: CopyBudget, bytes: number): void {
  assert(Number.isSafeInteger(bytes) && bytes >= 0, "host_observation_budget_invariant");
  assert(Number.isSafeInteger(budget.bytes) && budget.bytes >= 0,
    "host_observation_budget_invariant");
  if (budget.bytes + bytes > 32768) reject();
  budget.bytes += bytes;
}

export function copyObservationValue(
  value: unknown, budget: CopyBudget = { nodes: 0, bytes: 0 }, depth = 0,
): JsonValue {
  assert(Number.isSafeInteger(budget.nodes) && budget.nodes >= 0 && budget.nodes <= 1024,
    "host_observation_budget_invariant");
  assert(Number.isSafeInteger(budget.bytes) && budget.bytes >= 0 && budget.bytes <= 32768,
    "host_observation_budget_invariant");
  assert(Number.isSafeInteger(depth) && depth >= 0 && depth <= 9,
    "host_observation_depth_invariant");
  if (budget.nodes === 1024 || depth > 8) reject();
  budget.nodes++;
  if (value === null || typeof value === "boolean" || typeof value === "number" ||
      typeof value === "string") {
    if (typeof value === "string") text(value, 16384, true);
    if (typeof value === "number" &&
        (!Number.isFinite(value) || Math.abs(value) > Number.MAX_SAFE_INTEGER)) reject();
    charge(budget, Buffer.byteLength(JSON.stringify(value), "utf8"));
    return value;
  }
  if (Array.isArray(value)) {
    if (value.length > 64 || Reflect.ownKeys(value).length !== value.length + 1) reject();
    charge(budget, 2 + Math.max(0, value.length - 1));
    return Array.from({ length: value.length }, (_, index) => {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor?.enumerable || !("value" in descriptor)) return reject();
      return copyObservationValue(descriptor.value, budget, depth + 1);
    });
  }
  const body = observationRecord(value);
  const keys = Object.keys(body);
  charge(budget, 2 + Math.max(0, keys.length - 1));
  return Object.fromEntries(keys.map((key) => {
    text(key, 256, true);
    charge(budget, Buffer.byteLength(JSON.stringify(key), "utf8") + 1);
    return [key, copyObservationValue(body[key], budget, depth + 1)];
  }));
}

function list(value: unknown, maximum: number): unknown[] {
  if (!Array.isArray(value) || value.length > maximum) reject();
  return value;
}

function agentOutput(value: unknown, child = false): void {
  const required = child ? ["agent", "status", "rounds"] : ["mode", "report", "status", "rounds"];
  const optional = child ? ["runId", "children"]
    : ["agent", "tracePath", "runId", "logPath", "children"];
  const body = shape(value, required, optional);
  if (!child && body.mode !== "worker" && body.mode !== "rlm") reject();
  text(body.status, 256);
  number(body.rounds);
  for (const key of ["agent", "report", "tracePath", "runId", "logPath"]) {
    if (body[key] !== undefined) text(body[key], key === "report" ? 16384 : 4096, true);
  }
  if (body.children !== undefined) {
    for (const item of list(body.children, 32)) agentOutput(item, true);
  }
}

function resultMetadata(body: DataRecord): void {
  if (body.detail !== undefined) text(body.detail, 16384, true);
  if (body.detailLines !== undefined) {
    for (const value of list(body.detailLines, 32)) {
      const line = shape(value, ["key", "displayValue", "highlighted"], ["kind"]);
      text(line.key, 1024, true);
      text(line.displayValue, 16384, true);
      if (typeof line.highlighted !== "boolean" || (line.kind !== undefined &&
          !["added", "removed", "neutral"].includes(line.kind as string))) reject();
    }
  }
  if (body.agentOutput !== undefined) agentOutput(body.agentOutput);
}

function toolPayload(body: DataRecord): void {
  if (body.type === "tool:requested") {
    text(body.turnId, 256);
    if (list(body.calls, 32).length === 0) reject();
    for (const value of body.calls as unknown[]) {
      const call = shape(value, ["id", "name", "input"]);
      text(call.id, 256);
      text(call.name, 1024);
    }
    return;
  }
  if (body.type === "agent:child_tool_start" || body.type === "agent:child_tool_result") {
    text(body.agentToolCallId, 256);
    if (body.type === "agent:child_tool_result") {
      text(body.childToolCallId, 256);
      if (body.status !== "done" && body.status !== "error") reject();
    } else {
      const call = shape(body.call, ["id", "name", "inputSummary", "status"]);
      text(call.id, 256);
      text(call.name, 1024);
      text(call.inputSummary, 16384, true);
      if (call.status !== "running") reject();
    }
    return;
  }
  text(body.toolCallId, 256);
  if (body.type === "tool:retracted") return;
  text(body.name, 1024);
  for (const key of ["inputSummary", "title", "error"]) {
    if (Object.hasOwn(body, key)) text(body[key], 16384, true);
  }
  if (body.agentMode !== undefined && body.agentMode !== "worker" && body.agentMode !== "rlm") {
    reject();
  }
  if (body.type === "tool:result") resultMetadata(body);
}

function normalizeToolObservation(event: DataRecord): ObservationFrame {
  const [required, optional] = TOOL_FIELDS[event.type as string]!;
  shape(event, ["type", ...required], optional);
  const defined = Object.fromEntries(Object.entries(event).filter(([key, value]) =>
    value !== undefined || !optional.includes(key)));
  const body = copyObservationValue(defined) as DataRecord;
  toolPayload(body);
  return { kind: "tool_observation", scope: body.type === "tool:requested"
    ? "explicit_sdk_turn" : "turn_unattributed", event: body as ToolObservation };
}

function normalizeUsageObservation(value: unknown): ObservationFrame {
  const body = shape(value, [], USAGE_FIELDS);
  const entries = Object.entries(body).filter(([, value]) => value !== undefined);
  const usage = Object.fromEntries(entries.map(([key, value]) => [key,
    key === "model" || key === "provider" ? text(value, 1024, true)
      : number(value, key !== "costUsd")]));
  return { kind: "usage_observation", scope: "turn_unattributed",
    provenance: "sdk_reported", accounting: "UNKNOWN", usage };
}

export function normalizeSdkObservation(value: unknown): ObservationFrame {
  const event = observationRecord(value);
  const type = text(event.type, 128);
  if (Object.hasOwn(TOOL_FIELDS, type)) return normalizeToolObservation(event);
  if (type === "usage") {
    shape(event, ["type", "usage"]);
    return normalizeUsageObservation(event.usage);
  }
  if (type === "tool:progress") {
    shape(event, ["type", "toolCallId", "data"]);
    return { kind: "observation_gap", scope: "turn_unattributed", source_type: type,
      reason: "unqualified_progress", tool_call_id: text(event.toolCallId, 256) };
  }
  return { kind: "observation_gap", scope: "turn_unattributed", source_type: type,
    reason: "unsupported_event" };
}

export function validateObservationFrame(value: unknown): ObservationFrame {
  const body = observationRecord(value);
  if (body.kind === "tool_observation") {
    shape(body, ["kind", "scope", "event"]);
    const normalized = normalizeSdkObservation(body.event);
    if (normalized.kind !== body.kind || normalized.scope !== body.scope) reject();
    return normalized;
  }
  if (body.kind === "usage_observation") {
    shape(body, ["kind", "scope", "provenance", "accounting", "usage"]);
    if (body.scope !== "turn_unattributed" || body.provenance !== "sdk_reported" ||
        body.accounting !== "UNKNOWN") reject();
    return normalizeUsageObservation(body.usage);
  }
  if (body.kind !== "observation_gap" || body.scope !== "turn_unattributed") reject();
  if (body.reason === "unqualified_progress" && body.source_type === "tool:progress") {
    shape(body, ["kind", "scope", "source_type", "reason", "tool_call_id"]);
    return normalizeSdkObservation({ type: "tool:progress",
      toolCallId: body.tool_call_id, data: null });
  }
  shape(body, ["kind", "scope", "source_type", "reason"]);
  const normalized = normalizeSdkObservation({ type: text(body.source_type, 128) });
  if (body.reason !== "unsupported_event" || normalized.kind !== "observation_gap" ||
      normalized.reason !== body.reason) reject();
  return normalized;
}
