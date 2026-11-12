import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const model = await import("../src/screens/demo/model.ts");
const words = await import("../src/screens/demo/words.ts");
const { DemoPageView, DemoEntryView } = await import("../src/screens/demo/offer.tsx");
const { DemoGuideView } = await import("../src/screens/demo/guide.tsx");
const router = await import("../src/ui/router.ts");
const api = await import("../src/api-demo.ts");

const idle = () => {};
const steps = (patch = {}) => ({ created: false, goal_sent: false, agent_started: false, question_answered: false, change_set_accepted: false, ...patch });
const demo = (patch = {}) => ({
  project_id: null,
  name: "CrazyAgent demo",
  created_ms: null,
  goal: "Add a dark mode switch to the page and a test for it.",
  steps: steps(),
  pools: [],
  ...patch,
});
const member = (id, status = "signed_in") => ({ id, kind: "claude", provider: "Claude", status, status_label: status });
const pool = (id, name, members) => ({ id, name, summary: "", members, project_count: 0 });
const account = (id, name, status = "signed_in") => ({ id, name, provider: "claude", provider_name: "Claude", method: "subscription", method_label: "", url: null, model: null, status, status_label: status });
const ok = (value) => ({ kind: "ok", value });
const page = (props) => renderToStaticMarkup(createElement(DemoPageView, { picked: null, pending: false, error: null, onPick: idle, onCreate: idle, ...props }));
const text = (html) => html.replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

test("the demo page shows the cost note before any pool, and the note says any account in the pool may be used", () => {
  const view = demo({ pools: [{ pool: "pool-a", unavailable: null }] });
  const choices = model.demoPoolChoices(view, [pool("pool-a", "Main", [member("acc-1")])], [account("acc-1", "Work Claude")]);
  const html = page({ view: ok(view), choices: ok(choices) });
  const note = html.indexOf(words.DEMO_COST_NOTE.slice(0, 40));
  const firstPool = html.indexOf('type="radio"');
  assert.ok(note >= 0, "cost note shown");
  assert.ok(firstPool > note, "cost note comes before the first pool");
  assert.match(words.DEMO_COST_NOTE, /any account in that pool may be used/);
  assert.ok(text(html).includes(words.DEMO_ABOUT));
});

test("no pool is picked for the owner, even when exactly one pool can run the demo, and Create stays off until one is", () => {
  const view = demo({ pools: [{ pool: "pool-a", unavailable: null }] });
  const choices = model.demoPoolChoices(view, [pool("pool-a", "Main", [member("acc-1")])], [account("acc-1", "Work Claude")]);
  const html = page({ view: ok(view), choices: ok(choices) });
  assert.doesNotMatch(html, /checked=""/);
  assert.match(html, /<button(?=[^>]*disabled="")[^>]*>[^<]*(<[^>]+>)*Create the demo/);
  assert.equal(model.pickable(choices, null), false);
  assert.equal(model.pickable(choices, "pool-a"), true);
  const picked = page({ view: ok(view), choices: ok(choices), picked: "pool-a" });
  assert.match(picked, /<input(?=[^>]*value="pool-a")(?=[^>]*checked="")[^>]*>/);
  assert.doesNotMatch(picked, /<button(?=[^>]*disabled="")[^>]*>[^<]*(<[^>]+>)*Create the demo/);
});

test("a pool that runs only on rcli or has no ready account is listed disabled with its reason and cannot be picked", () => {
  const view = demo({ pools: [
    { pool: "pool-r", unavailable: "demo_harness_unavailable" },
    { pool: "pool-n", unavailable: "demo_pool_not_ready" },
    { pool: "pool-a", unavailable: null },
  ] });
  const pools = [pool("pool-r", "Local models", [member("acc-r")]), pool("pool-n", "Signed out", [member("acc-n", "not_signed_in")]), pool("pool-a", "Main", [member("acc-1")])];
  const choices = model.demoPoolChoices(view, pools, [account("acc-r", "Ollama", "key_saved"), account("acc-n", "Old", "not_signed_in"), account("acc-1", "Work Claude")]);
  assert.deepEqual(choices.map((choice) => [choice.id, choice.reason]), [
    ["pool-r", words.DEMO_POOL_REASONS.demo_harness_unavailable],
    ["pool-n", words.DEMO_POOL_REASONS.demo_pool_not_ready],
    ["pool-a", null],
  ]);
  assert.equal(model.pickable(choices, "pool-r"), false);
  assert.equal(model.pickable(choices, "pool-n"), false);
  assert.equal(model.pickable(choices, "gone"), false);
  const html = page({ view: ok(view), choices: ok(choices), picked: "pool-r" });
  assert.match(html, /<input(?=[^>]*value="pool-r")(?=[^>]*disabled="")[^>]*>/);
  assert.match(html, /<input(?=[^>]*value="pool-n")(?=[^>]*disabled="")[^>]*>/);
  assert.doesNotMatch(html, /<input(?=[^>]*value="pool-a")(?=[^>]*disabled="")[^>]*>/);
  assert.ok(text(html).includes("rcli"), "the rcli reason is on the page, not hidden");
  assert.match(html, /<button(?=[^>]*disabled="")[^>]*>[^<]*(<[^>]+>)*Create the demo/);
});

test("an unknown pool code still reads as a sentence that names the code", () => {
  const view = demo({ pools: [{ pool: "pool-x", unavailable: "demo_something_new" }] });
  const [choice] = model.demoPoolChoices(view, [], []);
  assert.equal(choice.name, "pool-x");
  assert.match(choice.reason, /demo_something_new/);
});

test("each pool shows its ready accounts by the names Accounts gives them, joined by account id", () => {
  const view = demo({ pools: [{ pool: "pool-a", unavailable: null }] });
  const pools = [pool("pool-a", "Main", [member("acc-1"), member("acc-2"), member("acc-3", "not_signed_in"), member("acc-gone")])];
  const accounts = [account("acc-1", "Work"), account("acc-2", "Work"), account("acc-3", "Spare", "not_signed_in")];
  const [choice] = model.demoPoolChoices(view, pools, accounts);
  assert.equal(choice.name, "Main");
  assert.deepEqual(choice.ready, ["Work (acc-1)", "Work (acc-2)"]);
  const html = page({ view: ok(view), choices: ok([choice]) });
  assert.ok(text(html).includes("Work (acc-1)"));
  assert.ok(!text(html).includes("Spare"));
});

test("with no pool the page links to Create a pool and offers no Create the demo", () => {
  const html = page({ view: ok(demo()), choices: ok([]) });
  assert.ok(text(html).includes(words.DEMO_NO_POOL));
  assert.match(html, new RegExp(`href="${router.formatRoute({ name: "settings", tab: "pools", action: "new" }).replace(/[?]/g, "\\?")}"`));
  assert.doesNotMatch(html, /Create the demo/);
  assert.doesNotMatch(html, /type="radio"/);
});

test("once the demo exists the Home entry and the demo page open it instead of making another", () => {
  const made = demo({ project_id: "crazyagent-demo", created_ms: 1, steps: steps({ created: true }) });
  const conversation = router.formatRoute({ name: "project", id: "crazyagent-demo", tab: "conversation" });
  assert.deepEqual(model.demoEntry(ok(made)), { action: "open", label: words.DEMO_ENTRY.open, href: conversation });
  assert.deepEqual(model.demoEntry(ok(demo())), { action: "start", label: words.DEMO_ENTRY.start, href: router.formatRoute({ name: "demo" }) });
  assert.equal(model.demoEntry({ kind: "loading" }), null);
  const html = page({ view: ok(made), choices: ok([]) });
  assert.ok(text(html).includes(words.DEMO_EXISTS));
  assert.match(html, new RegExp(`href="${conversation}"`));
  assert.doesNotMatch(html, /type="radio"/);
  assert.doesNotMatch(html, /Create the demo/);
  const entry = renderToStaticMarkup(createElement(DemoEntryView, { entry: model.demoEntry(ok(made)) }));
  assert.ok(text(entry).includes(words.DEMO_ENTRY.title));
  assert.ok(text(entry).includes(words.DEMO_ENTRY.body));
  assert.match(entry, new RegExp(`href="${conversation}"[^>]*>[^<]*(<[^>]+>)*${words.DEMO_ENTRY.open}`));
});

test("the demo page waits for its reads and says when the engine is not ready, never showing a pool it has not read", () => {
  assert.doesNotMatch(page({ view: { kind: "loading" }, choices: { kind: "loading" } }), /type="radio"/);
  assert.doesNotMatch(page({ view: { kind: "not_ready" }, choices: { kind: "not_ready" } }), /type="radio"/);
  const failed = page({ view: { kind: "error", code: "owner_read_failed", message: "x" }, choices: { kind: "loading" } });
  assert.doesNotMatch(failed, /type="radio"/);
  assert.match(failed, /role="alert"|Couldn/);
});

test("a refused Create the demo says why in the demo's own words, and an unknown code is named", () => {
  for (const [code, sentence] of Object.entries(words.DEMO_REFUSALS)) assert.equal(model.refusalCopy({ code }), sentence, code);
  assert.match(model.refusalCopy({ code: "demo_brand_new" }), /demo_brand_new/);
  const html = page({ view: ok(demo({ pools: [{ pool: "pool-a", unavailable: null }] })), choices: ok([{ id: "pool-a", name: "Main", ready: [], reason: null }]), error: words.DEMO_REFUSALS.demo_name_taken });
  assert.match(html, /role="alert"/);
  assert.ok(text(html).includes(words.DEMO_REFUSALS.demo_name_taken));
});

test("the guide shows only on the demo project's page and ticks each step from the engine's demo read", () => {
  assert.equal(model.guideSteps(demo(), "crazyagent-demo"), null);
  assert.equal(model.guideSteps(demo({ project_id: "crazyagent-demo" }), "another"), null);
  const guide = model.guideSteps(demo({ project_id: "crazyagent-demo", steps: steps({ created: true, goal_sent: true, agent_started: true }) }), "crazyagent-demo");
  assert.deepEqual(guide.steps.map((step) => [step.id, step.done]), [["goal", true], ["agents", true], ["question", false], ["changes", false], ["delete", false]]);
  assert.equal(guide.current, 2);
  assert.deepEqual(guide.steps.map((step) => [step.title, step.body]), words.DEMO_STEPS.map((step) => [step.title, step.body]));
  const all = model.guideSteps(demo({ project_id: "crazyagent-demo", steps: steps({ created: true, goal_sent: true, agent_started: true, question_answered: true, change_set_accepted: true }) }), "crazyagent-demo");
  assert.equal(all.current, 4, "the delete step is never ticked, so it is current at the end");
  const tab = (id, name) => router.formatRoute({ name: "project", id, tab: name });
  assert.deepEqual(guide.steps.slice(0, 4).map((step) => step.href), [
    tab("crazyagent-demo", "conversation"), tab("crazyagent-demo", "agents"), tab("crazyagent-demo", "questions"), tab("crazyagent-demo", "changes"),
  ]);
});

test("the guide's last step opens Remove for Delete everything, the one delete path there is", () => {
  const guide = model.guideSteps(demo({ project_id: "crazy agent/demo" }), "crazy agent/demo");
  assert.equal(guide.steps[4].href, `#/projects/${encodeURIComponent("crazy agent/demo")}/settings?section=remove&ask=remove`);
});

test("the guide panel shows the engine's goal with a copy action, the current step first and each step's tick", () => {
  const view = demo({ project_id: "crazyagent-demo", steps: steps({ created: true, goal_sent: true }) });
  const html = renderToStaticMarkup(createElement(DemoGuideView, { guide: model.guideSteps(view, "crazyagent-demo"), goal: view.goal, copied: false, onCopy: idle }));
  assert.ok(text(html).includes(words.DEMO_GUIDE_TITLE));
  assert.ok(text(html).includes(view.goal));
  assert.match(html, /Copy goal/);
  assert.match(html, /<li[^>]*aria-current="step"[^>]*>(?:(?!<\/li>)[\s\S])*Watch the agents work/);
  assert.equal((html.match(/aria-current="step"/g) ?? []).length, 1);
  assert.match(html, /<li[^>]*aria-current="step"[^>]*>(?:(?!<\/li>)[\s\S])*href="#\/projects\/crazyagent-demo\/agents"[^>]*>Open Agents/);
  assert.equal((html.match(/data-done="true"/g) ?? []).length, 1);
  for (const step of words.DEMO_STEPS) assert.ok(text(html).includes(step.title), step.id);
});

test("the demo mark reads the engine's demo project, not the project's name", () => {
  const made = ok(demo({ project_id: "crazyagent-demo" }));
  assert.equal(model.isDemoProject(made, "crazyagent-demo"), true);
  assert.equal(model.isDemoProject(made, "my-app"), false);
  assert.equal(model.isDemoProject(ok(demo()), "crazyagent-demo"), false);
  assert.equal(model.isDemoProject({ kind: "loading" }, "crazyagent-demo"), false);
});

test("the demo page has its own route", () => {
  assert.equal(router.formatRoute({ name: "demo" }), "#/demo");
  assert.deepEqual(router.parseRoute("#/demo"), { name: "demo" });
  assert.deepEqual(router.parseRoute("#/demo/extra"), { name: "home" });
});

test("the demo reads and Create the demo go through their own native commands", async () => {
  const source = readFileSync(new URL("../src/api-demo.ts", import.meta.url), "utf8");
  assert.match(source, /call<DemoView>\("desktop_project_demo"\)/);
  assert.match(source, /call<IntentReceipt>\("desktop_create_demo", \{ pool, generation \}\)/);
  assert.equal((await api.loadDemo()).kind, "error");
  assert.equal((await api.createDemo("pool-a", 1)).kind, "error");
});

const receipt = (patch = {}) => ({ kind: "create_demo", operation_id: "op-1", project_id: "crazyagent-demo", outcome: "applied", pending: false, rejection: null, ...patch });
const steps2 = () => {
  const calls = [];
  let generation = 0;
  return {
    calls,
    deps: (patch = {}) => ({
      create: async (pool, gen) => { calls.push(`create ${pool} ${gen}`); return ok(receipt()); },
      reconcile: async () => { calls.push("reconcile"); return ok(receipt()); },
      read: async () => { calls.push("read"); return ok(demo()); },
      assign: async (project, pool, gen) => { calls.push(`assign ${project} ${pool} ${gen}`); return ok({ kind: "applied" }); },
      generation: () => { generation += 1; return generation; },
      ...patch,
    }),
  };
};

test("Create the demo sends the picked pool, then gives that same pool to the new demo, as New Project does", async () => {
  const run = steps2();
  assert.deepEqual(await model.makeDemo("pool-a", run.deps()), { kind: "made", project: "crazyagent-demo", note: null });
  assert.deepEqual(run.calls, ["create pool-a 1", "assign crazyagent-demo pool-a 2"]);
});

test("a refused or unconfirmed Create the demo gives no pool and says why", async () => {
  const refused = steps2();
  const made = await model.makeDemo("pool-a", refused.deps({ create: async (pool) => { refused.calls.push(`create ${pool}`); return ok(receipt({ outcome: "rejected", rejection: "demo_name_taken" })); } }));
  assert.deepEqual(made, { kind: "refused", message: words.DEMO_REFUSALS.demo_name_taken });
  assert.deepEqual(refused.calls, ["create pool-a"]);
  const failed = steps2();
  const lost = await model.makeDemo("pool-a", failed.deps({ create: async () => ({ kind: "error", code: "demo_pool_not_ready", message: "x" }) }));
  assert.deepEqual(lost, { kind: "refused", message: words.DEMO_REFUSALS.demo_pool_not_ready });
  assert.ok(!failed.calls.some((call) => call.startsWith("assign")));
  const unsure = steps2();
  const pending = receipt({ pending: true, outcome: "unknown" });
  const left = await model.makeDemo("pool-a", unsure.deps({ create: async () => ok(pending), reconcile: async () => { unsure.calls.push("reconcile"); return ok(pending); } }));
  assert.deepEqual(left, { kind: "refused", message: words.DEMO_UNCONFIRMED });
  assert.deepEqual(unsure.calls, ["reconcile", "read"]);
});

test("an unconfirmed Create the demo reads the demo again and gives the pool once the demo is there, as the TUI does", async () => {
  const pending = receipt({ pending: true, outcome: "unknown" });
  const unsure = (run, patch = {}) => run.deps({
    create: async (pool, gen) => { run.calls.push(`create ${pool} ${gen}`); return ok(pending); },
    reconcile: async () => { run.calls.push("reconcile"); return ok(pending); },
    read: async () => { run.calls.push("read"); return ok(demo({ project_id: "crazyagent-demo", created_ms: 1, steps: steps({ created: true }) })); },
    ...patch,
  });
  const run = steps2();
  assert.deepEqual(await model.makeDemo("pool-a", unsure(run)), { kind: "made", project: "crazyagent-demo", note: null });
  assert.deepEqual(run.calls, ["create pool-a 1", "reconcile", "read", "assign crazyagent-demo pool-a 2"]);
  const refused = steps2();
  const unassigned = await model.makeDemo("pool-a", unsure(refused, { assign: async () => ok({ kind: "rejected", code: "settings_read_only" }) }));
  assert.deepEqual(unassigned, { kind: "made", project: "crazyagent-demo", note: words.DEMO_POOL_UNASSIGNED });
  const unread = steps2();
  const blind = await model.makeDemo("pool-a", unsure(unread, { read: async () => { unread.calls.push("read"); return { kind: "error", code: "owner_read_failed", message: "x" }; } }));
  assert.deepEqual(blind, { kind: "refused", message: words.DEMO_UNCONFIRMED });
  assert.ok(!unread.calls.some((call) => call.startsWith("assign")), "an unread demo gets no pool");
  const nameless = steps2();
  const applied = await model.makeDemo("pool-a", unsure(nameless, { create: async () => ok(receipt({ project_id: null })) }));
  assert.deepEqual(applied, { kind: "made", project: "crazyagent-demo", note: null });
  assert.deepEqual(nameless.calls, ["read", "assign crazyagent-demo pool-a 2"]);
});

test("the demo page reads the demo again for an unconfirmed Create the demo", () => {
  const source = readFileSync(new URL("../src/screens/demo/offer.tsx", import.meta.url), "utf8");
  assert.match(source, /read: loadDemo,/);
});

test("a demo made whose pool could not be given still opens, with a line saying to choose the pool again", async () => {
  const run = steps2();
  const made = await model.makeDemo("pool-a", run.deps({ assign: async () => ok({ kind: "rejected", code: "settings_read_only" }) }));
  assert.deepEqual(made, { kind: "made", project: "crazyagent-demo", note: words.DEMO_POOL_UNASSIGNED });
  const failed = await model.makeDemo("pool-a", steps2().deps({ assign: async () => ({ kind: "error", code: "invalid_project", message: "x" }) }));
  assert.equal(failed.note, words.DEMO_POOL_UNASSIGNED);
});

test("a lost Create the demo reply is reconciled once and a settled answer is kept", async () => {
  const run = steps2();
  const made = await model.makeDemo("pool-a", run.deps({ create: async (pool, gen) => { run.calls.push(`create ${pool} ${gen}`); return ok(receipt({ pending: true, outcome: "unknown" })); } }));
  assert.deepEqual(made, { kind: "made", project: "crazyagent-demo", note: null });
  assert.deepEqual(run.calls, ["create pool-a 1", "reconcile", "assign crazyagent-demo pool-a 2"]);
});

test("one failed demo read keeps the last good read, so the guide stays and its poll goes on", () => {
  const open = demo({ project_id: "crazyagent-demo", created_ms: 1, steps: steps({ created: true, goal_sent: true }) });
  const failed = { kind: "error", code: "owner_busy", message: "x" };
  assert.deepEqual(model.keptDemo(ok(open), null), ok(open));
  assert.deepEqual(model.keptDemo(failed, open), ok(open));
  assert.deepEqual(model.keptDemo({ kind: "loading" }, open), ok(open));
  assert.deepEqual(model.keptDemo(failed, null), failed, "with no good read the failure shows");
  assert.deepEqual(model.keptDemo({ kind: "not_ready" }, open), { kind: "not_ready" }, "an engine with no demo is not hidden");
  const fresh = demo({ project_id: "crazyagent-demo", created_ms: 1, steps: steps({ created: true, goal_sent: true, agent_started: true }) });
  assert.deepEqual(model.keptDemo(ok(fresh), open), ok(fresh));
  assert.equal(model.demoPolls(model.keptDemo(failed, open)), true);
  assert.equal(model.demoPolls(ok(demo())), false, "no demo, nothing to follow");
  assert.equal(model.demoPolls(ok(demo({ project_id: "crazyagent-demo", steps: steps({ change_set_accepted: true }) }))), false);
  assert.equal(model.demoPolls(failed), false);
  const guide = renderToStaticMarkup(createElement(DemoGuideView, {
    guide: model.guideSteps(model.keptDemo(failed, open).value, "crazyagent-demo"), goal: open.goal, copied: false, onCopy: idle,
  }));
  assert.match(guide, /Step 2 of 5/);
});

test("useDemo shows the kept read and re-arms its poll on every read, failed ones too", () => {
  const source = readFileSync(new URL("../src/screens/demo/offer.tsx", import.meta.url), "utf8");
  const hook = source.slice(source.indexOf("export function useDemo"), source.indexOf("export function DemoEntryView"));
  assert.match(hook, /keptDemo\(latest, /);
  assert.match(hook, /demoPolls\(view\)/);
  assert.match(hook, /\[polls, latest\]/);
});
