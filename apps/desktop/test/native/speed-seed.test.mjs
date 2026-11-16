import assert from "node:assert/strict";
import test from "node:test";
import { speedReads } from "./speed-seed.mjs";

test("speedReads times every ticket read before the project open that leaves the ticket stale", () => {
  const calls = speedReads({ project: "speed-project", sessions: 50 }, "ticket-1");
  const commands = calls.map(({ cmd }) => cmd);
  assert.deepEqual([...commands].sort(), ["desktop_master_sessions", "desktop_master_state", "desktop_open_project"]);
  const opened = commands.indexOf("desktop_open_project");
  assert.equal(opened, calls.length - 1, `the project open is timed last: ${commands.join(", ")}`);
  assert.deepEqual(calls[opened].args, { id: "speed-project" });
  for (const { cmd, args } of calls.slice(0, opened)) assert.deepEqual(args, { ticket: "ticket-1" }, `${cmd} reads on the ticket`);
});

test("speedReads refuses a side with no project or a missing ticket", () => {
  assert.throws(() => speedReads({ project: " " }, "ticket-1"), /a seeded project/);
  assert.throws(() => speedReads({ project: "speed-project" }, ""), /a project ticket/);
});
