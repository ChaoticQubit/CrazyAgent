import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { AppView } = await import("../src/app.tsx");
const { updateStore } = await import("../src/screens/settings/updates-state.ts");

const profile = { name: "Mira Okafor", role: "", bio: "", preferred_stack: [], working_style: { detail: "terse", initiative: "ask_first" } };

function session(patch = {}) {
  const forbidden = () => { throw new Error("static rendering must not call the engine"); };
  return {
    engine: "running", unconfirmed: false, profile: { kind: "ok", value: profile }, projects: { kind: "ok", value: [] },
    toast: "", revision: 1, retry: forbidden, checkUnconfirmed: forbidden, refreshProfile: forbidden, refreshProjects: forbidden,
    showToast: forbidden, noteResult: (result) => result, ...patch,
  };
}

const UPDATES = { name: "settings", tab: "updates" };
const render = (patch, route = { name: "home" }) => renderToStaticMarkup(createElement(AppView, { session: session(patch), route }));

function standalone(markup) {
  assert.match(markup, /<h1>Updates<\/h1>/);
  assert.match(markup, /class="full-state"/);
  assert.match(markup, />Back<\/button>/);
  assert.doesNotMatch(markup, /engine isn&#x27;t running|Tell CrazyAgent about you|class="sidebar"/);
}

test("the engine-down page offers Check for updates, and names a release a check already found", () => {
  const down = render({ engine: "down" });
  assert.match(down, /<h1>CrazyAgent&#x27;s engine isn&#x27;t running<\/h1>/);
  assert.match(down, />Check for updates<\/button>/);
  assert.doesNotMatch(down, /is available/);
  updateStore.dispatch({ kind: "check_done", update: { version: "0.3.0", current_version: "0.2.0", notes: null }, at: 1 });
  try {
    assert.match(render({ engine: "down" }), /CrazyAgent 0\.3\.0 is available\./);
  } finally {
    updateStore.dispatch({ kind: "channel_changed" });
  }
});

test("first run offers Check for updates before the profile is saved", () => {
  const welcome = render({ profile: { kind: "ok", value: null } });
  assert.match(welcome, /<h1>Tell CrazyAgent about you<\/h1>/);
  assert.match(welcome, />Check for updates<\/button>/);
});

test("Settings, Updates opens on its own while the engine is down, launching, or before first run finishes", () => {
  standalone(render({ engine: "down" }, UPDATES));
  standalone(render({ engine: "connecting" }, UPDATES));
  standalone(render({ profile: { kind: "loading" } }, UPDATES));
  standalone(render({ profile: { kind: "ok", value: null } }, UPDATES));
});

test("with the engine running and a profile saved, Updates stays inside Settings in the shell", () => {
  const markup = render({}, UPDATES);
  assert.match(markup, /class="sidebar"/);
  assert.match(markup, /<h1>Settings<\/h1>/);
  assert.match(markup, /href="#\/settings\/updates" aria-selected="true"/);
  assert.doesNotMatch(markup, />Back<\/button>/);
});
