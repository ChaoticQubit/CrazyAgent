import assert from "node:assert/strict";
import { encodeEvent } from "./host.codec.ts";
import { SdkTurnProjection } from "./host.turns.ts";

const projection = new SdkTurnProjection();
const events = [
  { type: "tool:requested", turnId: "native/turn", calls: [
    { id: "call/reused", name: "Read", input: { path: "a", nested: [null, false, -0.125] } },
  ] },
  { type: "tool:pending", toolCallId: "call/reused", name: "Read", inputSummary: "" },
  { type: "tool:start", toolCallId: "call/reused", name: "Read", inputSummary: "", agentMode: "worker" },
  { type: "tool:retracted", toolCallId: "call/reused" },
  { type: "tool:start", toolCallId: "call/reused", name: "Read", inputSummary: "retry" },
  { type: "tool:result", toolCallId: "call/reused", name: "Read", title: "", detail: "",
    detailLines: [{ key: "", displayValue: "", highlighted: false, kind: "neutral" }],
    agentOutput: { mode: "rlm", report: "", status: "completed", rounds: 0,
      agent: "", tracePath: "", runId: "", logPath: "", children: [
        { agent: "", status: "done", rounds: 0, runId: "", children: [] },
      ] } },
  { type: "tool:error", toolCallId: "call/reused", name: "Read", error: "" },
  { type: "agent:child_tool_start", agentToolCallId: "parent", call: {
    id: "child", name: "Read", inputSummary: "", status: "running",
  } },
  { type: "agent:child_tool_result", agentToolCallId: "parent", childToolCallId: "child", status: "done" },
  { type: "agent:child_tool_result", agentToolCallId: "parent", childToolCallId: "child", status: "error" },
  { type: "usage", usage: {} },
  { type: "usage", usage: { inputTokens: 0, outputTokens: 1, totalTokens: 0,
    cachedTokens: 0, cacheWriteTokens: 0, costUsd: 0.0125, contextTokens: 0,
    contextLimit: 9007199254740991, model: "", provider: "" } },
  { type: "usage", usage: { costUsd: 0 } },
  { type: "tool:progress", toolCallId: "call/reused", data: { unqualified: true } },
  { type: "future:event", unqualified: true },
];
process.stdout.write(encodeEvent("session-a", "instance-a", 1n, { kind: "ready" }));
let sequence = 1n;
for (const event of events) {
  const bodies = projection.event(event);
  assert.equal(bodies.length, 1);
  process.stdout.write(encodeEvent("session-a", "instance-a", ++sequence, bodies[0]));
}
