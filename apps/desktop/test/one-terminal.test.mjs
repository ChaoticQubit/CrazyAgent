import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/one-terminal.mjs", import.meta.url).pathname);

/** Mounts the Agents tab in a 1200x800 page with the engine answering attaches after
 * `delayMs`, runs each step in turn (`["card", name]` clicks a graph card, `["waiting", name]`
 * clicks Open terminal on a "Waiting on you" row in the toolbar's questions drawer), waiting for that agent's terminal to go live
 * when `settle` is set, then waits for the page to settle. Inputs: the steps, the delay and
 * whether to wait. Output: what the page and the engine show. */
async function run(steps, delayMs, settle) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${delayMs})`);
    assert.equal(await evaluate("Fixture.until(() => document.querySelectorAll('.tg-card-open').length === 4 && document.querySelector('.tg-asks')?.textContent === '2 ask you')"), true, "the graph and the inbox drew");
    for (const [kind, name] of steps) {
      const step = kind === "card" ? `Fixture.click(${JSON.stringify(name)})` : `Fixture.openWaiting(${JSON.stringify(name)})`;
      assert.equal(await evaluate(step), true, `${kind} ${name}`);
      if (settle) {
        assert.equal(await evaluate(`Fixture.until(() => Fixture.shown().panels.includes(${JSON.stringify(`${name} terminal`)}) && Fixture.shown().live >= 1)`), true, `${name} went live`);
      }
    }
    const last = `${steps.at(-1)[1]} terminal`;
    await evaluate(`Fixture.until(() => Fixture.shown().attaches.length === ${steps.length} && Fixture.shown().panels.includes(${JSON.stringify(last)}) && Fixture.shown().live > 0)`);
    await evaluate(`new Promise((done) => setTimeout(done, ${delayMs + 300}))`);
    return evaluate("Fixture.shown()");
  });
}

test("clicking three agents in the graph leaves one terminal panel, for the last one, and one attached terminal client", async () => {
  const seen = await run([["card", "Ada"], ["card", "Brook"], ["card", "Cove"]], 0, true);
  assert.deepEqual(seen.panels, ["Cove terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-3"], JSON.stringify(seen));
  assert.deepEqual(seen.detaches, ["team-1", "team-2"], JSON.stringify(seen));
});

test("clicking three agents faster than the engine attaches still leaves one panel and one attached client", async () => {
  const seen = await run([["card", "Ada"], ["card", "Brook"], ["card", "Cove"]], 400, false);
  assert.deepEqual(seen.panels, ["Cove terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-3"], JSON.stringify(seen));
});

test("a terminal opened from Waiting on you is the same one panel the graph swaps, so clicking two agents after it leaves one", async () => {
  const seen = await run([["waiting", "Ada"], ["card", "Brook"], ["card", "Cove"]], 0, true);
  assert.deepEqual(seen.panels, ["Cove terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-3"], JSON.stringify(seen));
  assert.deepEqual(seen.detaches, ["team-1", "team-2"], JSON.stringify(seen));
});

test("an agent waiting on you that the graph does not draw opens in the same one panel, and a graph click replaces it", async () => {
  const seen = await run([["waiting", "Dune"], ["card", "Ada"]], 0, true);
  assert.deepEqual(seen.panels, ["Ada terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-1"], JSON.stringify(seen));
  assert.deepEqual(seen.detaches, ["team-5"], JSON.stringify(seen));
});

test("Open terminal in Waiting on you opens the one panel while the team graph is loading, failed, not in the build or empty", async () => {
  for (const graph of ["pending", "error", "not_ready", "empty"]) {
    const seen = await withPage(async ({ evaluate, send }) => {
      await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
      await evaluate(fixture);
      await evaluate(`Fixture.mount(0, ${JSON.stringify(graph)})`);
      assert.equal(await evaluate("Fixture.until(() => document.querySelector('.tg-asks')?.textContent === '2 ask you')"), true, `${graph}: the inbox read`);
      assert.equal(await evaluate("Fixture.openWaiting('Ada')"), true, `${graph}: Open terminal on Ada`);
      await evaluate("Fixture.until(() => Fixture.shown().panels.includes('Ada terminal') && Fixture.shown().live > 0)");
      return evaluate("Fixture.shown()");
    });
    assert.deepEqual(seen.panels, ["Ada terminal"], `${graph}: ${JSON.stringify(seen)}`);
    assert.equal(seen.xterms, 1, `${graph}: ${JSON.stringify(seen)}`);
    assert.deepEqual(seen.clients, ["team-1"], `${graph}: ${JSON.stringify(seen)}`);
  }
});

test("a panel opened from Waiting on you before the graph reads keeps its terminal and its draft once the graph draws that agent", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mount(0, 'late')");
    assert.equal(await evaluate("Fixture.until(() => document.querySelector('.tg-asks')?.textContent === '2 ask you')"), true, "the inbox read");
    assert.equal(await evaluate("Fixture.openWaiting('Ada')"), true, "Open terminal on Ada");
    assert.equal(await evaluate("Fixture.until(() => Fixture.shown().live > 0 && document.querySelector('.tg-composer-input') !== null)"), true, "Ada's terminal went live");
    assert.equal(await evaluate("Fixture.type('Keep the menu on the home page')"), true, "the owner typed a draft");
    await evaluate("Fixture.release()");
    assert.equal(await evaluate("Fixture.until(() => document.querySelectorAll('.tg-card-open').length === 4)"), true, "the graph drew");
    await evaluate("new Promise((done) => setTimeout(done, 300))");
    return evaluate("Fixture.shown()");
  });
  assert.deepEqual(seen.panels, ["Ada terminal"], JSON.stringify(seen));
  assert.equal(seen.draft, "Keep the menu on the home page", JSON.stringify(seen));
  assert.deepEqual(seen.attaches, ["team-1"], `one attach, no remount: ${JSON.stringify(seen)}`);
  assert.deepEqual(seen.detaches, [], JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-1"], JSON.stringify(seen));
});

test("the panel's Message puts the cursor in its Type to box, its Stop asks first, and Pause all beside Open all terminals pauses the team", async () => {
  const steps = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mount(0, 'ok', [Fixture.runningTask(7)])");
    assert.equal(await evaluate("Fixture.until(() => document.querySelectorAll('.tg-card-open').length === 4)"), true, "the graph drew");
    assert.equal(await evaluate("Fixture.click('Ada')"), true, "card Ada");
    assert.equal(await evaluate("Fixture.until(() => Fixture.shown().live > 0)"), true, "Ada's terminal went live");
    const seen = {};
    assert.equal(await evaluate("Fixture.press('.tg-terminal-head', 'Message')"), true, "Message in the panel head");
    seen.message = await evaluate("Fixture.around()");
    assert.equal(await evaluate("Fixture.press('.tg-terminal-head', 'Stop')"), true, "Stop in the panel head");
    seen.stop = await evaluate("Fixture.around()");
    assert.equal(await evaluate("Fixture.press('dialog[open]', 'Cancel')"), true, "Cancel the stop");
    assert.equal(await evaluate("Fixture.press('.tg-head', 'Pause all')"), true, "Pause all in the graph head");
    seen.pause = await evaluate("Fixture.around()");
    assert.equal(await evaluate("Fixture.press('dialog[open]', 'Pause all agents')"), true, "confirm the pause");
    await evaluate("Fixture.until(() => Fixture.around().notes.length > 0)");
    seen.paused = await evaluate("Fixture.around()");
    return seen;
  });
  assert.equal(steps.message.focused, "Type to Ada...", JSON.stringify(steps.message));
  assert.equal(steps.stop.dialog?.title, "Stop Ada?", JSON.stringify(steps.stop));
  const head = steps.pause.head;
  assert.equal(head[head.indexOf("Open all terminals") - 1], "Pause all", `Pause all sits right before Open all terminals: ${JSON.stringify(head)}`);
  const gap = steps.pause.headGaps[head.indexOf("Open all terminals")];
  assert.ok(gap >= 0 && gap <= 16, `Pause all sits beside Open all terminals, not across the head: ${gap} px`);
  assert.equal(steps.pause.dialog?.title, "Pause all agents?", JSON.stringify(steps.pause));
  assert.match(steps.pause.dialog?.text ?? "", /1 running task stops\./u);
  assert.deepEqual(steps.paused.cancels, [null], `one pause of every task: ${JSON.stringify(steps.paused)}`);
  assert.deepEqual(steps.paused.notes, ["Paused. Running tasks are stopping and waiting tasks are cancelled."], JSON.stringify(steps.paused));
});

/** Mounts the Agents tab at 1200x800 with a running task and a codex sub-agent harness, waits
 * until the graph drew and the questions inbox read, runs the steps and returns what each
 * step saw. Input: the steps as page expressions, by name. Output: what each returned. */
async function agentsPage(steps) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(0, 'ok', [Fixture.runningTask(7)], { harnesses: [{ harness: "codex", role: "sub", agents_max: 2, tools: true, live: 1 }] })`);
    assert.equal(await evaluate("Fixture.until(() => document.querySelectorAll('.tg-card-open').length === 4)"), true, "the graph drew");
    await evaluate("Fixture.until(() => document.querySelector('.tg-asks')?.textContent.includes('ask you'))");
    const seen = {};
    for (const [name, step] of steps) seen[name] = await evaluate(step);
    return seen;
  });
}

test("the Agents toolbar is the mockup's one row: Team and Agent settings, the team line, Running and All, Pause all and Open all terminals", async () => {
  const { bar } = await agentsPage([["bar", "Fixture.toolbar()"]]);
  assert.deepEqual(bar.names, ["Team", "Agent settings", "Master and 3 agents · 2 ask you · Pool A", "Running", "All", "Pause all", "Open all terminals"], JSON.stringify(bar));
  assert.equal(bar.rows, 1, `one row: ${JSON.stringify(bar)}`);
  assert.ok(bar.gap !== null && bar.gap <= 24, `the graph starts right under the row: ${bar.gap} px`);
  assert.equal(bar.questions, false, "no Questions for you section on the page");
});

test("under the graph the Team view keeps the team's agents, tasks, Hand a task, Stop all, wake rules and usage in a closed Agents and tasks fold", async () => {
  const { parts } = await agentsPage([["parts", "Fixture.folds()"]]);
  assert.deepEqual(parts.folds.find((fold) => fold.label === "Agents and tasks"), { label: "Agents and tasks", open: false }, JSON.stringify(parts));
  assert.deepEqual(parts.parts, {
    "Stop all": "Agents and tasks", "Hand a task": "Agents and tasks", "Team agents": "Agents and tasks", "Team tasks": "Agents and tasks", "Wake rules": "Agents and tasks", "Usage": "Agents and tasks",
  }, JSON.stringify(parts));
});

test("N ask you opens a drawer with Questions for you and Sub-agent events, and Open terminal there closes it and opens the one panel", async () => {
  const seen = await agentsPage([
    ["pressed", "Fixture.press('.tg-summary', '2 ask you')"],
    ["drawer", "Fixture.until(() => Fixture.drawer() !== null).then(() => Fixture.drawer())"],
    ["opened", "Fixture.openWaiting('Ada')"],
    ["live", "Fixture.until(() => Fixture.shown().panels.includes('Ada terminal') && Fixture.shown().live > 0)"],
    ["after", "Fixture.drawer()"],
  ]);
  assert.equal(seen.pressed, true, "2 ask you is a button in the team line");
  assert.equal(seen.drawer?.title, "Questions for you", JSON.stringify(seen.drawer));
  assert.deepEqual(seen.drawer?.sections, ["Questions for you", "Sub-agent events"], JSON.stringify(seen.drawer));
  assert.equal(seen.opened, true, "Open terminal on Ada in the drawer");
  assert.equal(seen.live, true, "Ada's terminal opened in the one panel");
  assert.equal(seen.after, null, "the drawer closed");
});
