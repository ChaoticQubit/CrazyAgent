import assert from "node:assert/strict";

/** The part of the Team editor's warning the owner reported in OP-41 (DEC-114): "<pool> has no
 * account Claude Code can use. Add an account it can use to the pool, or pick another harness." */
export const NO_ACCOUNT = "has no account";

/** Judges the OP-41 check of trial 3 on what the owner sees: the project's pool card in
 * Settings > Pool and the master part of Settings > Team with Claude Code picked as the master.
 * Input: { pool: { name, summary, readiness }, master, warnings, help }, the card's name,
 * account summary and readiness lines, the master harness shown, the warning lines under the
 * master and its help line. Output: the step detail. Throws "not run: ..." when the pool holds
 * no Claude account, and a failure when the editor reads the pool as having none for Claude Code. */
export function poolFitVerdict({ pool, master, warnings, help }) {
  assert(typeof pool?.name === "string" && pool.name.length > 0, "the pool card has a name");
  assert(typeof pool.summary === "string" && typeof pool.readiness === "string", "the pool card has its summary and readiness");
  assert(Array.isArray(warnings) && warnings.every((line) => typeof line === "string"), "the warnings are lines");
  const claude = /(?:^|, )(\d+) Claude(?:,|$)/u.exec(pool.summary);
  if (!claude || Number(claude[1]) === 0) throw new Error(`not run: ${pool.name} holds no Claude account (${pool.summary})`);
  if (master !== "Claude Code") throw new Error(`the master harness reads ${master}, not Claude Code`);
  const refused = warnings.map((line) => line.trim()).find((line) => line.includes(NO_ACCOUNT));
  if (refused) throw new Error(`OP-41: the Team editor says "${refused}" for ${pool.summary}`);
  const expected = `Uses an account from ${pool.name}.`;
  if (help?.trim() !== expected) throw new Error(`the master help reads "${help}", not "${expected}"`);
  return `${pool.name} (${pool.summary}; ${pool.readiness}) fits Claude Code as master: no "${NO_ACCOUNT}" warning, and the help reads "${expected}"`;
}
