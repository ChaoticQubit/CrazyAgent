import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { CHECK_INTERVAL_MS, reduceUpdate, progressLabel, progressPercent, updateMessage, shouldAnnounce, createUpdateStore } = await import("../src/screens/settings/updates-state.ts");
const { UpdatesView } = await import("../src/screens/settings/updates.tsx");
const { parseRoute, formatRoute } = await import("../src/ui/router.ts");

const found = { version: "0.2.0-dev.202610251600", current_version: "0.2.0-dev.202610241200", notes: "Faster startup." };
const idle = { kind: "idle" };
const run = (events, from = idle) => events.reduce(reduceUpdate, from);
const forbidden = () => { throw new Error("static rendering must not call the app"); };
const props = (patch = {}) => ({ settings: { kind: "ok", value: { current_version: "0.2.0-dev.202610241200", channel: "dev" } }, phase: idle, channelBusy: false, channelProblem: "",
  cli: { kind: "idle" }, onChannel: forbidden, onCheck: forbidden, onInstall: forbidden, onRestart: forbidden, onLinkCli: forbidden, onRetry: forbidden, ...patch });
const render = (patch) => renderToStaticMarkup(createElement(UpdatesView, props(patch)));

test("a check finds a newer release or says the app is current", () => {
  assert.deepEqual(run([{ kind: "check_started" }]), { kind: "checking" });
  assert.deepEqual(run([{ kind: "check_started" }, { kind: "check_done", update: found, at: 5 }]), { kind: "available", update: found, checkedAt: 5 });
  assert.deepEqual(run([{ kind: "check_started" }, { kind: "check_done", update: null, at: 7 }]), { kind: "current", checkedAt: 7 });
  const failed = run([{ kind: "check_started" }, { kind: "check_failed", code: "update_check_failed" }]);
  assert.equal(failed.kind, "failed");
  assert.equal(failed.update, null);
  assert.match(failed.message, /update server/);
});

test("installing needs a found release and the owner's click, and reports each step", () => {
  assert.deepEqual(run([{ kind: "install_started" }]), idle);
  assert.deepEqual(run([{ kind: "check_done", update: null, at: 1 }, { kind: "install_started" }]), { kind: "current", checkedAt: 1 });
  const installing = run([{ kind: "check_done", update: found, at: 1 }, { kind: "install_started" }]);
  assert.deepEqual(installing, { kind: "installing", update: found, step: { kind: "downloading", downloaded: 0, total: null } });
  const half = reduceUpdate(installing, { kind: "progress", progress: { kind: "downloading", downloaded: 5_242_880, total: 10_485_760 } });
  assert.equal(progressPercent(half.step), 50);
  assert.equal(progressLabel(half.step), "Downloading 5 MB of 10 MB (50%)");
  assert.equal(progressLabel({ kind: "downloading", downloaded: 2048, total: null }), "Downloading 2 KB");
  assert.equal(progressPercent({ kind: "downloading", downloaded: 2048, total: null }), null);
  assert.equal(progressPercent({ kind: "downloading", downloaded: 20, total: 10 }), 100);
  assert.equal(progressLabel({ kind: "verifying" }), "Checking the signature...");
  assert.equal(progressLabel({ kind: "stopping_engine" }), "Stopping the engine. Your agents pick up where they were after the restart.");
  assert.equal(progressLabel({ kind: "installing" }), "Installing...");
  assert.deepEqual(reduceUpdate(half, { kind: "install_done", engineStarted: true }), { kind: "installed", update: found, engineStarted: true });
});

test("a failed download or bad signature shows the error, changes nothing and offers the same release again", () => {
  const installing = run([{ kind: "check_done", update: found, at: 1 }, { kind: "install_started" }]);
  const failed = reduceUpdate(installing, { kind: "install_failed", code: "update_signature_invalid" });
  assert.deepEqual(failed, { kind: "failed", update: found, message: updateMessage("update_signature_invalid") });
  assert.match(failed.message, /signature/);
  assert.match(failed.message, /[Nn]othing (was|changed)/);
  assert.match(updateMessage("update_download_failed"), /Nothing changed/);
  assert.match(updateMessage("sidecar_missing"), /command line tool/);
  assert.match(updateMessage("update_too_large"), /Nothing changed/);
  assert.notEqual(updateMessage("update_too_large"), updateMessage("something_new"));
  assert.equal(updateMessage("something_new"), "Something went wrong with the update. Try again.");
  assert.equal(reduceUpdate(failed, { kind: "install_started" }).kind, "installing");
});

test("checks and channel switches never interrupt an install in progress or finished", () => {
  const installing = run([{ kind: "check_done", update: found, at: 1 }, { kind: "install_started" }]);
  for (const event of [{ kind: "check_started" }, { kind: "check_done", update: null, at: 2 }, { kind: "check_failed", code: "update_check_failed" }, { kind: "channel_changed" }]) {
    assert.equal(reduceUpdate(installing, event), installing);
  }
  const installed = reduceUpdate(installing, { kind: "install_done", engineStarted: false });
  assert.equal(reduceUpdate(installed, { kind: "check_started" }), installed);
  assert.equal(reduceUpdate(installed, { kind: "progress", progress: { kind: "installing" } }), installed);
  assert.deepEqual(run([{ kind: "check_done", update: found, at: 1 }, { kind: "channel_changed" }]), idle);
});

test("the app checks every 6 hours and announces each new version once", () => {
  assert.equal(CHECK_INTERVAL_MS, 6 * 60 * 60 * 1000);
  assert.equal(shouldAnnounce(null, found), true);
  assert.equal(shouldAnnounce(found.version, found), false);
  assert.equal(shouldAnnounce("0.1.0", found), true);
  assert.equal(shouldAnnounce("0.1.0", null), false);
});

test("the shared update store tells subscribers only about real changes", () => {
  const store = createUpdateStore();
  let calls = 0;
  const stop = store.subscribe(() => { calls += 1; });
  store.dispatch({ kind: "install_started" });
  assert.equal(calls, 0);
  store.dispatch({ kind: "check_started" });
  assert.equal(calls, 1);
  assert.deepEqual(store.get(), { kind: "checking" });
  stop();
  store.dispatch({ kind: "check_done", update: null, at: 3 });
  assert.equal(calls, 1);
  assert.deepEqual(store.get(), { kind: "current", checkedAt: 3 });
});

test("Settings has an Updates tab", () => {
  assert.deepEqual(parseRoute("#/settings/updates"), { name: "settings", tab: "updates" });
  assert.equal(formatRoute({ name: "settings", tab: "updates" }), "#/settings/updates");
});

test("the Updates page shows the version, the channel switch, Check now and the command line tool", () => {
  const markup = render();
  assert.match(markup, /0\.2\.0-dev\.202610241200/);
  assert.match(markup, /Stable/);
  assert.match(markup, /Dev/);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>Dev</);
  assert.match(markup, /role="radio" aria-checked="false"[^>]*>Stable</);
  assert.match(markup, /Check now/);
  assert.match(markup, /Install command line tool/);
  assert.doesNotMatch(markup, /Download and install/);
  assert.doesNotMatch(markup, /Restart/);
});

test("the Updates page offers Download and install only for a found release, with progress while installing", () => {
  const available = render({ phase: { kind: "available", update: found, checkedAt: 1 } });
  assert.match(available, /CrazyAgent 0\.2\.0-dev\.202610251600 is available/);
  assert.match(available, /Faster startup\./);
  assert.match(available, /Download and install/);
  const installing = render({ phase: { kind: "installing", update: found, step: { kind: "downloading", downloaded: 5_242_880, total: 10_485_760 } } });
  assert.match(installing, /<progress[^>]*value="50"/);
  assert.match(installing, /Downloading 5 MB of 10 MB \(50%\)/);
  assert.doesNotMatch(installing, /Check now/);
  const installed = render({ phase: { kind: "installed", update: found, engineStarted: false } });
  assert.match(installed, /Restart now/);
  assert.match(installed, /engine starts again when CrazyAgent restarts/);
  const failed = render({ phase: { kind: "failed", update: found, message: updateMessage("update_signature_invalid") } });
  assert.match(failed, /role="alert"/);
  assert.match(failed, /Try again/);
});

test("the Updates page stays plain while loading, missing from the build or failing", () => {
  assert.doesNotMatch(render({ settings: { kind: "loading" } }), /Check now/);
  assert.match(render({ settings: { kind: "not_ready" } }), /Not ready yet|isn.t in this build/);
  assert.match(render({ settings: { kind: "error", code: "update_channel_invalid", message: "The saved update channel isn't valid." } }), /saved update channel/);
  assert.match(render({ cli: { kind: "done", message: "Linked crazyagent into ~/.local/bin." } }), /Linked crazyagent/);
  assert.match(render({ cli: { kind: "failed", message: "The command line tool couldn't be installed." } }), /role="alert"/);
});
