import assert from "node:assert/strict";

/** The plugins whose hooks the owner named in OP-13 (DEC-114): caveman and the ADHD reply style. */
export const OWNER_HOOK_PLUGINS = /caveman|adhd/iu;

/** Judges trial 4 check N-D22 on what Settings, Plugins and skills, Hooks shows. Input:
 * { plugins, failedFirst, failedAfterRetry }: the plugin name of each listed hook, whether the
 * list first said "Couldn't load hooks." and whether it still did after Try again. Output: the
 * step detail. Throws with the owner's problem when the list never loads (OP-23), is empty or
 * holds no hook of caveman or the ADHD style (OP-13). */
export function hooksVerdict({ plugins, failedFirst, failedAfterRetry }) {
  assert(Array.isArray(plugins) && plugins.every((name) => typeof name === "string"), "the plugins are names");
  assert(typeof failedFirst === "boolean" && typeof failedAfterRetry === "boolean", "the load outcomes are booleans");
  assert(failedFirst || !failedAfterRetry, "Try again is pressed only after a failed load");
  if (failedAfterRetry) throw new Error("OP-23: the Hooks list still says Couldn't load hooks. after Try again");
  if (plugins.length === 0) throw new Error("OP-13: the Hooks list says No hooks yet");
  const listed = [...new Set(plugins)];
  const owners = listed.filter((name) => OWNER_HOOK_PLUGINS.test(name));
  if (owners.length === 0) throw new Error(`OP-13: no hook of caveman or the ADHD style is listed; it lists ${listed.join(", ")}`);
  const load = failedFirst ? "the first load failed and Try again loaded it" : "it loaded on the first open, so Try again was not needed";
  return `${plugins.length} hooks listed from ${listed.join(", ")}; the owner's ${owners.join(" and ")} hooks are there; ${load}`;
}

/** The message trial 4 check N-D24 sends the master: one project fact holding the token, to
 * be saved with the remember tool. Input: the token, letters, digits and dashes. Output: the text. */
export function rememberPrompt(token) {
  assert.match(token, /^[A-Za-z0-9-]{4,40}$/u, "the token is plain");
  return `Use your remember tool now, with scope project, to save this fact for this project word for word, then answer done: ${token} is the trial 4 project memory fact.`;
}

/** Judges trial 4 check N-D24 on what the owner sees. Input: { token, writes, source, approved,
 * shown }: the token the fact holds, the Agent memory writes setting (save or ask), the source
 * line Settings, Memory shows under the fact before any approval, whether the trial approved it
 * there and the text of Context, Project memory afterwards. Output: the step detail. Throws
 * when the fact is not the master's, waits unapproved under Ask me first, or is not in Context,
 * Project memory (OP-22). */
export function memoryVerdict({ token, writes, source, approved, shown }) {
  assert(["save", "ask"].includes(writes), "Agent memory writes is save or ask");
  assert(typeof source === "string" && typeof shown === "string" && typeof approved === "boolean", "the readings are text");
  if (!/\b(?:saved|asked) by Master$/u.test(source)) throw new Error(`the fact in Settings, Memory is not the master's: ${source}`);
  const waited = source.startsWith("Waiting for your approval.");
  if (waited && !approved) throw new Error(`the master's fact still waits for approval: ${source}`);
  if (!shown.includes(token)) throw new Error(`OP-22: Context, Project memory does not show the master's fact ${token}`);
  const how = waited ? "it waited for approval under Ask me first and was approved in Settings, Memory" : "it was saved at once";
  return `Agent memory writes: ${writes === "ask" ? "Ask me first" : "Save directly"}; the master saved ${token} with remember (${source}); ${how}; Context, Project memory shows it`;
}
