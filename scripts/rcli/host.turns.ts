import assert from "node:assert/strict";
import type { AgentEvent, TurnStatus } from "../../.local-state/tools/rcli-source/src/core/bus/events.ts";
import { FRAME_TEXT_MAX, identifier, type Control, type EventBody } from "./host.codec.ts";
import { SdkMessageProjection } from "./host.messages.ts";
import { normalizeSdkObservation, observationRecord } from "./host.observations.ts";
type InputFrame = Extract<EventBody, { kind: "input" }>;
type PendingInput = {
id: string;
questions: { question: string; options: string[] }[];
answers: string[];
promptId: string;
awaitingResponse: boolean;
};
type TurnState = {
id: string;
started: boolean;
sdkId: string | null;
ended: boolean;
cancelling: boolean;
messages: SdkMessageProjection | null;
input: PendingInput | null;
};
export type HostAction =
| { kind: "submit"; turnId: string; text: string; steer: boolean }
| { kind: "cancel" | "shutdown" }
| { kind: "answer"; events: EventBody[]; delivery: { id: string; answers: string[] } | null };
function reject(): never { throw new Error("host_turn_rejected"); }
function bounded(value: unknown, maximum: number): string {
if (typeof value !== "string" || value.length === 0 || !value.isWellFormed() || Buffer.byteLength(value, "utf8") > maximum) reject();
return value;
}
export class SdkTurnProjection {
private readonly turns = new Map<string, TurnState>();
private readonly requestIds = new Set<string>();
private readonly turnIds = new Set<string>();
private readonly sdkIds = new Set<string>();
private readonly inputIds = new Set<string>();
private readonly abortedInputs = new Set<string>();
private awaitingStart: string | null = null;
private nextInput = 1;
private stopped = false;
private disposed = false;
private check(): void {
assert(this.turns.size <= 2 && [...this.turns].every(([id, turn]) => turn !== null && typeof turn === "object" && id === turn.id && this.turnIds.has(id)), "host_turn_state_invariant");
assert(this.awaitingStart === null || this.turns.has(this.awaitingStart), "host_turn_state_invariant");
assert([this.requestIds, this.turnIds, this.sdkIds, this.inputIds].every((ids) => ids.size <= 4096) && Number.isSafeInteger(this.nextInput) && this.nextInput >= 1 && this.nextInput <= 4097, "host_turn_state_invariant");
assert(this.abortedInputs.size <= this.inputIds.size && [...this.abortedInputs].every((id) => this.inputIds.has(id)), "host_turn_state_invariant");
}
get hasOutstanding(): boolean { this.check(); return this.turns.size !== 0; }
admit(value: Control): HostAction {
this.check();
if (this.stopped || this.disposed || this.requestIds.size >= 4096 || this.requestIds.has(value.request_id)) reject();
identifier(value.request_id);
const body = value.control;
let action: HostAction;
switch (body.kind) {
case "prompt":
case "steer": {
identifier(body.turn_id);
if (this.turnIds.size >= 4096 || this.turnIds.has(body.turn_id)) reject();
if (body.kind === "prompt" ? this.turns.size !== 0 : this.turns.size !== 1 || [...this.turns.values()][0]!.cancelling) reject();
if (body.kind === "steer") {
for (const turn of this.turns.values()) { turn.cancelling = true; this.cancelInput(turn); }
}
this.turnIds.add(body.turn_id);
this.turns.set(body.turn_id, { id: body.turn_id, started: false, sdkId: null, ended: false, cancelling: false, messages: null, input: null });
action = { kind: "submit", turnId: body.turn_id, text: body.text, steer: body.kind === "steer" };
break;
}
case "cancel": {
if (!this.turns.has(body.turn_id)) reject();
for (const turn of this.turns.values()) { turn.cancelling = true; this.cancelInput(turn); }
action = { kind: "cancel" };
break;
}
case "answer": {
const turn = this.turns.get(body.turn_id);
const pending = turn?.input;
if (!turn || turn.cancelling || !pending || pending.awaitingResponse || body.prompt_id !== pending.promptId || body.answers.length !== 1) reject();
const response = bounded(body.answers[0], 1024);
pending.answers.push(response);
if (pending.answers.length === pending.questions.length) {
pending.awaitingResponse = true;
action = { kind: "answer", events: [], delivery: { id: pending.id, answers: [...pending.answers] } };
} else {
pending.promptId = this.inputId();
action = { kind: "answer", delivery: null, events: [this.inputFrame(turn, pending)] };
}
break;
}
case "shutdown":
this.stop();
action = { kind: "shutdown" };
break;
default: return reject();
}
this.requestIds.add(value.request_id);
this.check();
return action;
}
start(turnId: string): void {
this.check();
const turn = this.turns.get(turnId);
if (this.disposed || !turn || turn.started || this.awaitingStart !== null || [...this.turns.values()].some((value) => value.sdkId !== null && !value.ended)) reject();
turn.started = true;
this.awaitingStart = turnId;
}
event(event: AgentEvent): EventBody[] {
this.check();
if (this.disposed) reject();
observationRecord(event);
switch (event.type) {
case "turn:start": {
if (this.awaitingStart === null || this.sdkIds.size >= 4096) reject();
const nativeId = bounded(event.turnId, 256);
if (this.sdkIds.has(nativeId)) reject();
const turn = this.turns.get(this.awaitingStart)!;
assert(turn.started && turn.sdkId === null, "host_turn_state_invariant");
const messages = new SdkMessageProjection(turn.id, nativeId);
turn.sdkId = nativeId;
turn.messages = messages;
this.sdkIds.add(nativeId);
this.awaitingStart = null;
return [];
}
case "assistant:delta":
case "assistant:message":
case "assistant:message:discard": {
const turn = this.running(event.turnId);
assert(turn.messages !== null, "host_turn_state_invariant");
return turn.messages.accept(event);
}
case "turn:end": {
const turn = this.running(event.turnId);
turn.ended = true;
turn.input = null;
return [];
}
case "turn:cancelled": {
const turn = this.running(event.turnId);
turn.cancelling = true;
this.cancelInput(turn);
return [];
}
case "input:request": {
const turn = this.running();
if (this.stopped || turn.cancelling || turn.input !== null || this.inputIds.size >= 4096) reject();
const id = bounded(event.request.id, 256);
if (this.inputIds.has(id) || !Array.isArray(event.request.questions) || event.request.questions.length < 1 || event.request.questions.length > 8 || this.nextInput + event.request.questions.length > 4097) reject();
const questions = event.request.questions.map((question) => {
const text = bounded(question.question, 4096);
if (!Array.isArray(question.options) || question.options.length > 8) reject();
const options = question.options.map((option) => bounded(option, 1024));
if (Buffer.byteLength(JSON.stringify([text, ...options]), "utf8") > FRAME_TEXT_MAX) reject();
return { question: text, options };
});
const pending: PendingInput = { id, questions, answers: [], promptId: this.inputId(), awaitingResponse: false };
turn.input = pending;
this.inputIds.add(id);
return [this.inputFrame(turn, pending)];
}
case "input:response": {
if (this.abortedInputs.has(event.response.id)) {
if (!Array.isArray(event.response.answers) || event.response.answers.length !== 0) reject();
this.abortedInputs.delete(event.response.id);
return [];
}
const turn = this.running();
const pending = turn.input;
if (!pending || !pending.awaitingResponse || event.response.id !== pending.id || !Array.isArray(event.response.answers) || event.response.answers.length !== pending.answers.length || event.response.answers.some((answer, index) => answer !== pending.answers[index])) reject();
turn.input = null;
return [];
}
default: return [normalizeSdkObservation(event)];
}
}
complete(turnId: string, status: TurnStatus): EventBody[] {
this.check();
const turn = this.turns.get(turnId);
if (this.disposed || !turn || status !== "completed" && status !== "cancelled" && status !== "failed" || status === "completed" && (turn.sdkId === null || !turn.ended)) reject();
turn.messages?.close();
turn.input = null;
this.turns.delete(turnId);
if (this.awaitingStart === turnId) this.awaitingStart = null;
return [{ kind: "completed", turn_id: turnId, status: status === "completed" ? "succeeded" : status }];
}
stop(): void {
this.check();
this.stopped = true;
for (const turn of this.turns.values()) { turn.cancelling = true; this.cancelInput(turn); }
}
close(): void {
this.stop();
for (const turn of this.turns.values()) turn.messages?.close();
this.turns.clear();
this.abortedInputs.clear();
this.awaitingStart = null;
this.disposed = true;
}
private cancelInput(turn: TurnState): void {
if (turn.input !== null) this.abortedInputs.add(turn.input.id);
turn.input = null;
}
private running(sdkId?: string): TurnState {
const matches = [...this.turns.values()].filter((turn) => turn.sdkId !== null && !turn.ended && (sdkId === undefined || turn.sdkId === sdkId));
if (matches.length !== 1) return reject();
return matches[0]!;
}
private inputId(): string {
assert(this.nextInput >= 1 && this.nextInput <= 4096, "host_turn_state_invariant");
return `input-${this.nextInput++}`;
}
private inputFrame(turn: TurnState, pending: PendingInput): InputFrame {
const question = pending.questions[pending.answers.length];
assert(question !== undefined, "host_turn_state_invariant");
return { kind: "input", turn_id: turn.id, prompt_id: pending.promptId, question: question.question, choices: [...question.options] };
}
}
