import { call, type Result } from "./api";

export type ChoosableHarness = "codex" | "claude-code";
export type Found =
  | { kind: "registered"; path: string }
  | { kind: "ready"; path: string }
  | { kind: "missing"; looked: string[] }
  | { kind: "refused"; path: string; code: string };

export const CHOOSABLE: readonly ChoosableHarness[] = ["codex", "claude-code"];
export const LOOKED_MAX = 48;

const FILE_NAME: Record<ChoosableHarness, string> = { codex: "codex", "claude-code": "claude" };
const REFUSED = "harness_path_refused";

const COPY: Record<string, string> = {
  harness_path_unknown_harness: "You can only choose the file for Codex and Claude Code.",
  harness_path_invalid: "That isn't a full path to one file.",
  harness_path_not_found: "There's no file there.",
  harness_path_not_executable: "That file isn't a program this Mac can run.",
  harness_path_agent_space: "That file is in CrazyAgent's own folders, where agents work, so CrazyAgent won't run it.",
  harness_path_shim: "That file is a Volta, asdf or mise shim, which starts another program. Choose the real file instead.",
  harness_path_changeable: "Another user on this Mac can change that file or a folder above it, so CrazyAgent won't run it.",
  harness_path_unreadable: "CrazyAgent couldn't read or save the chosen file. Try again.",
  native_auth_host_executable_unsafe: "Another user on this Mac can change that file, or it isn't a plain program file, so CrazyAgent won't run it.",
  native_auth_host_executable_changed: "That file changed while CrazyAgent checked it. Try again.",
  native_auth_host_unavailable: "The engine can't add a harness right now. Restart the engine, then try again.",
  native_auth_host_registration_frozen: "The engine is stopping, so it can't add a harness now. Try again once it has restarted.",
  owner_runtime_stale: "The engine restarted. Try again.",
  [REFUSED]: "CrazyAgent won't run that file.",
};

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Whether the owner may choose the file of this harness. Input: a catalog id. Output: true
 * for codex and claude-code, the two harnesses with a managed sign-in. */
export function choosable(id: string): id is ChoosableHarness {
  return (CHOOSABLE as readonly string[]).includes(id);
}

/** The file name the owner looks for when choosing a harness's file. Input: the id.
 * Output: "codex" or "claude". */
export function harnessFileName(id: ChoosableHarness): string {
  return FILE_NAME[id];
}

/** The plain sentence for why a found or chosen harness file was refused. Input: the code.
 * Output: one sentence, the same for no two known codes. */
export function pathCopy(code: string): string {
  return COPY[code] ?? COPY[REFUSED];
}

/** Whether a failure code is one a harness file look or choice answers with, so its own
 * sentence fits better than the general one. Input: the code. Output: true when known. */
export function isPathCode(code: string): boolean {
  return Object.hasOwn(COPY, code);
}

/** Has the engine look for one harness again and add it when it is found and passes its
 * checks; a harness already added is never replaced. Input: the id. Output: what the look
 * found, not ready, or a plain error. */
export function findHarness(id: ChoosableHarness): Promise<Result<Found>> {
  assert(choosable(id), "harness id");
  return call<Found>("desktop_harness_find", { id });
}

/** Stores the file the owner chose for one harness after the engine checks it, or forgets the
 * choice, then looks again. Inputs: the id and the absolute path, null to forget. Output: what
 * the look found, not ready, or a plain error. */
export function chooseHarnessPath(id: ChoosableHarness, path: string | null): Promise<Result<Found>> {
  assert(choosable(id), "harness id");
  assert(path === null || path.length > 0, "harness path");
  return call<Found>("desktop_harness_choose_path", { id, path });
}

/** Opens the system file chooser for a harness program file. Output: the chosen absolute
 * path, null when the owner cancelled, not ready, or a plain error. */
export function pickHarnessFile(): Promise<Result<string | null>> {
  return call<string | null>("desktop_pick_harness_file");
}
