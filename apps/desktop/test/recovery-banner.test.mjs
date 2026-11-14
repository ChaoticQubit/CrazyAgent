import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement, isValidElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const banner = await import("../src/screens/project/recovery-banner.tsx");
const api = await import("../src/api-recovery.ts");
const { Button } = await import("../src/ui/index.ts");

const words = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const never = (what) => () => { throw new Error(`${what} ran during render`); };

function run(agent, label, state, extra = {}) {
  return { agent, label, role: "sub", harness: "Codex", at_ms: 1_800_000_000_000, turn: 3, asked: "Plan the page", state, reason: null, paused: false, ...extra };
}

function notice(kind, id, shown) {
  const landing = kind === "landing_accept";
  return { action_kind: kind, id, agent: landing ? null : 2, label: landing ? null : "Ada", change_set: landing ? "set-1" : null, shown, started_ms: 1_800_000_000_000 };
}

const full = {
  runs: [
    run(2, "Ada", "interrupted"),
    run(5, "Grace", "resuming"),
    run(6, "Linus", "interrupted", { paused: true }),
    run(3, "Sub-agent 3", "failed", { reason: "The account is unavailable." }),
    run(4, "Sub-agent 4", "resumed"),
  ],
  notices: [notice("agent_spawn", 7, "unknown"), notice("landing_accept", 8, "landed"), notice("landing_accept", 9, "dropped")],
  more: 0,
};

function elements(node, found = []) {
  if (Array.isArray(node)) {
    for (const child of node) elements(child, found);
  } else if (isValidElement(node)) {
    found.push(node);
    elements(node.props.children, found);
  }
  return found;
}

function view(props) {
  return { banner: full, busy: null, note: "", onRetry: never("onRetry"), onDismiss: never("onDismiss"), ...props };
}

function rendered(props) {
  return banner.RecoveryBannerView(view(props));
}

test("the banner lists what was interrupted, what resumed and what failed, with the notices to check", () => {
  const markup = renderToStaticMarkup(createElement(banner.RecoveryBannerView, view()));
  const text = words(markup);
  assert.match(text, /Work was interrupted when CrazyAgent stopped\. 7 to resolve\./);
  const sections = [...markup.matchAll(/<section[^>]*aria-label="([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(sections, ["Interrupted", "Resumed", "Failed", "Check before you start again"]);
  assert.match(text, /Ada \(Codex\), turn 3: "Plan the page"\. Resumes on its own\./);
  assert.match(text, /Grace \(Codex\), turn 3: "Plan the page"\. Resuming now\./);
  assert.match(text, /Linus \(Codex\), turn 3: "Plan the page"\. Resumes when you resume the team\./);
  assert.match(text, /Sub-agent 4 \(Codex\) runs again\./);
  assert.match(text, /Sub-agent 3 \(Codex\) didn't resume\. The account is unavailable\./);
  assert.match(text, /Ada's start may or may not have run\. Check the agent, then dismiss this to let it start again\./);
  assert.match(text, /Change set set-1 landed while CrazyAgent stopped\./);
  assert.match(text, /Change set set-1 did not land\. Accept it again from Changes\./);
  const tree = elements(rendered());
  const buttons = tree.filter((element) => element.type === Button);
  assert.deepEqual(buttons.map((element) => element.props.children), ["Retry", "Dismiss", "Dismiss", "Dismiss"], "Retry only on the failed resume");
});

test("Retry and Dismiss call the engine for the item shown, and one choice runs at a time", () => {
  const chosen = [];
  const tree = elements(rendered({ onRetry: (agent) => chosen.push(["retry", agent]), onDismiss: (item) => chosen.push(["dismiss", item.action_kind, item.id]) }));
  const buttons = tree.filter((element) => element.type === Button);
  for (const button of buttons) button.props.onClick();
  assert.deepEqual(chosen, [["retry", 3], ["dismiss", "agent_spawn", 7], ["dismiss", "landing_accept", 8], ["dismiss", "landing_accept", 9]]);

  const busy = elements(rendered({ busy: banner.choiceKey({ agent: 3 }) })).filter((element) => element.type === Button);
  assert.equal(busy[0].props.pending, true);
  assert.ok(busy.every((element) => element.props.disabled), "every choice waits while one runs");

  const source = readFileSync(new URL("../src/api-recovery.ts", import.meta.url), "utf8");
  assert.match(source, /return call<null>\("desktop_retry_recovery", \{ agent \}\);/);
  assert.match(source, /return call<null>\("desktop_dismiss_recovery", \{ actionKind: notice\.action_kind, id: notice\.id \}\);/);
  assert.match(source, /return call<RecoveryBanner>\("desktop_recovery_banner", \{ projectId \}\);/);
  const page = readFileSync(new URL("../src/screens/project/recovery-banner.tsx", import.meta.url), "utf8");
  assert.match(page, /onRetry=\{\(agent\) => void choose\(choiceKey\(\{ agent \}\), \(\) => retryRecovery\(agent\)/);
  assert.match(page, /onDismiss=\{\(notice\) => void choose\(choiceKey\(notice\), \(\) => dismissRecovery\(notice\)/);
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /<RecoveryBanner key=\{projectId\} project=\{projectId\} onDeleteAgain=\{deleteAgain\} \/>/);
});

test("a delete an engine stop cut short says so and offers Keep the project and Delete again", () => {
  const cut = { action_kind: "project_delete", id: 11, agent: null, label: null, change_set: null, shown: "unknown", started_ms: 1_800_000_000_000 };
  const only = { runs: [], notices: [cut], more: 0 };
  const text = words(renderToStaticMarkup(createElement(banner.RecoveryBannerView, view({ banner: only, onDeleteAgain: never("onDeleteAgain") }))));
  assert.match(text, /A delete was cut short\. Some folders may already be in the Trash\./);
  assert.match(text, /If you keep the project, its agents stay stopped until you start them again with Start all again in the project menu\./);
  const chosen = [];
  const buttons = elements(rendered({
    banner: only,
    onDismiss: (item) => chosen.push(["keep", item.action_kind, item.id]),
    onDeleteAgain: () => chosen.push(["delete again"]),
  })).filter((element) => element.type === Button);
  assert.deepEqual(buttons.map((button) => button.props.children), ["Keep the project", "Delete again"]);
  for (const button of buttons) button.props.onClick();
  assert.deepEqual(chosen, [["keep", "project_delete", 11], ["delete again"]]);
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /<SettingsTab key=\{`\$\{settingsSection \?\? ""\}-\$\{deleteAsks\}`\} data=\{data\} section=\{settingsSection\} \/>/);
  const settings = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(settings, /const asked = hashParam\("ask"\);/);
  assert.match(settings, /useRef<RemoveChoice>\(asked === "delete" \? "everything" : "dashboard"\)/);
});

test("one agent's several runs each get their own row key in every group", () => {
  const twice = (state, extra = {}) => [run(3, "Sub-agent 3", state, { at_ms: 1_800_000_000_000, ...extra }), run(3, "Sub-agent 3", state, { at_ms: 1_800_000_060_000, ...extra })];
  const runs = [...twice("interrupted"), ...twice("resumed"), ...twice("failed", { reason: "The account is unavailable." })];
  const lists = elements(rendered({ banner: { runs, notices: [], more: 0 } })).filter((element) => element.type === "ul");
  assert.equal(lists.length, 3, "Interrupted, Resumed and Failed");
  for (const list of lists) {
    const keys = list.props.children.map((item) => item.key);
    assert.equal(keys.length, 2);
    assert.equal(new Set(keys).size, 2, `rows of one agent's two runs share a key: ${keys}`);
  }
});

test("an empty banner, or one where everything resumed, shows nothing and stops reading", () => {
  for (const quiet of [{ runs: [], notices: [], more: 0 }, { runs: [run(4, "Sub-agent 4", "resumed")], notices: [], more: 0 }]) {
    assert.equal(rendered({ banner: quiet }), null);
    assert.equal(renderToStaticMarkup(createElement(banner.RecoveryBannerView, view({ banner: quiet }))), "");
    assert.equal(banner.stillResuming(quiet), false);
  }
  const hidden = { runs: [], notices: [], more: 2 };
  assert.match(words(renderToStaticMarkup(createElement(banner.RecoveryBannerView, view({ banner: hidden })))), /2 more aren't shown/);
  assert.equal(banner.stillResuming(full), true);
  assert.equal(banner.stillResuming({ runs: [run(3, "Sub-agent 3", "failed")], notices: [], more: 0 }), false, "a failed resume waits for Retry, not a timer");
});

test("refusals read as plain sentences and unknown codes keep their code", () => {
  assert.equal(api.recoveryMessage({ kind: "error", code: "recovery_not_failed", message: "" }), "That resume is no longer failed. The list is read again.");
  assert.equal(api.recoveryMessage({ kind: "error", code: "ledger_not_notice", message: "" }), "That start is still being checked; it settles on its own.");
  assert.equal(api.recoveryMessage({ kind: "error", code: "owner_busy", message: "" }), "CrazyAgent is busy with other requests. Try again in a moment.");
  assert.equal(api.recoveryMessage({ kind: "not_ready" }), "Recovery isn't ready in this engine yet.");
  assert.match(api.recoveryMessage({ kind: "error", code: "recovery_mystery", message: "" }), /recovery_mystery/);
  const rust = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/recovery.rs", import.meta.url), "utf8");
  const codes = [...rust.slice(rust.indexOf("RECOVERY_CODES"), rust.indexOf("];", rust.indexOf("RECOVERY_CODES"))).matchAll(/"([a-z_]+)"/gu)].map((match) => match[1]);
  for (const code of ["recovery_not_failed", "ledger_finished", "ledger_missing", "ledger_not_notice", "recovery_record_invalid", "owner_busy"]) {
    assert.ok(codes.includes(code), `${code} is an engine code`);
  }
});

test("a failed read keeps the last banner in view and the reads going, and a failed first read offers Try again", () => {
  const resuming = { runs: [run(5, "Grace", "resuming")], notices: [], more: 0 };
  const busy = { kind: "error", code: "owner_busy", message: "" };
  assert.deepEqual(banner.latestBanner({ kind: "ok", value: full }, resuming), { banner: full, problem: "" }, "a good read replaces the last one");
  assert.deepEqual(banner.latestBanner({ kind: "loading" }, null), { banner: null, problem: "" });
  const kept = banner.latestBanner(busy, resuming);
  assert.equal(kept.banner, resuming, "the runs still resuming stay in view after one failed read");
  assert.equal(kept.problem, "Interrupted work couldn't be read. CrazyAgent is busy with other requests. Try again in a moment.");
  assert.equal(banner.stillResuming(kept.banner), true, "so the banner keeps reading until their resume settles");
  const first = banner.latestBanner(busy, null);
  assert.equal(first.banner, null);
  assert.match(first.problem, /CrazyAgent is busy with other requests/);
  for (const code of ["engine_unreachable", "not_connected"]) {
    assert.deepEqual(banner.latestBanner({ kind: "error", code, message: "" }, resuming), { banner: null, problem: "" }, `${code} is quiet; the reconnect reads again`);
  }
  assert.match(banner.latestBanner({ kind: "not_ready" }, null).problem, /Recovery isn't ready in this engine yet\./);

  const page = readFileSync(new URL("../src/screens/project/recovery-banner.tsx", import.meta.url), "utf8");
  assert.match(page, /const \{ banner, problem \} = latestBanner\(loaded, last\);\n  const polling = keepsReading\(banner, problem\);/);
  assert.match(page, /useEffect\(\(\) => \{ if \(loaded\.kind === "ok"\) setLast\(loaded\.value\); \}, \[loaded\]\);/);
  assert.match(page, /<Button size="small" icon="refresh-cw" onClick=\{reload\}>Try again<\/Button>/);
  assert.match(page, /note=\{\[note, problem\]\.filter\(Boolean\)\.join\(" "\)\}/);
});

test("a failed read after Retry or Dismiss, with nothing resuming, keeps the banner reading", () => {
  const failedOnly = { runs: [run(3, "Sub-agent 3", "failed", { reason: "The account is unavailable." })], notices: [], more: 0 };
  const busy = { kind: "error", code: "owner_busy", message: "" };
  const kept = banner.latestBanner(busy, failedOnly);
  assert.equal(kept.banner, failedOnly, "the old copy stays in view");
  assert.equal(banner.stillResuming(kept.banner), false, "nothing in it resumes");
  assert.equal(banner.keepsReading(kept.banner, kept.problem), true, "so the failed read itself keeps the reads going until the retried run's outcome shows");
  const read = banner.latestBanner({ kind: "ok", value: failedOnly }, failedOnly);
  assert.equal(banner.keepsReading(read.banner, read.problem), false, "a good read with only a failed run waits for Retry, not a timer");
  const resuming = { runs: [run(5, "Grace", "resuming")], notices: [], more: 0 };
  assert.equal(banner.keepsReading(resuming, ""), true);
  const first = banner.latestBanner(busy, null);
  assert.equal(banner.keepsReading(first.banner, first.problem), false, "a failed first read offers Try again instead");
  const quiet = banner.latestBanner({ kind: "error", code: "engine_unreachable", message: "" }, failedOnly);
  assert.equal(banner.keepsReading(quiet.banner, quiet.problem), false, "the reconnect reads again, not the timer");
});
