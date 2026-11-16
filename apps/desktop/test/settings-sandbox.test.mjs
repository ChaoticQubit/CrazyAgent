import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { LINUX_SOCKETS, LINUX_STRICT, LINUX_STRICT_DEFAULT, LINUX_SUPERVISOR, LINUX_SUPERVISOR_DEFAULT, SandboxSettingsView, sandboxDraft } = await import("../src/screens/settings/sandbox.tsx");
const { knownError } = await import("../src/api.ts");
const { SETTINGS_TABS } = await import("../src/ui/router.ts");

const ok = (linux_sockets) => ({ kind: "ok", value: { linux_sockets } });
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const view = (settings, extra = {}) => renderToStaticMarkup(createElement(SandboxSettingsView, {
  settings, picked: null, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const checked = (markup) => [...markup.matchAll(/<input type="radio" name="linux-sockets"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z_]+)"/)[1]);
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);
const stored = (linux_sockets, linux_supervisor, linux_strict = "off") => ({ kind: "ok", value: { linux_sockets, linux_supervisor, linux_strict } });
const supervised = (markup) => [...markup.matchAll(/<input type="radio" name="linux-supervisor"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z_]+)"/)[1]);
const strictly = (markup) => [...markup.matchAll(/<input type="radio" name="linux-strict"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z_]+)"/)[1]);

test("Settings, Sandbox lists every Linux sockets option with what it allows and risks", () => {
  assert.ok(SETTINGS_TABS.includes("sandbox"));
  const markup = view(ok("tcp_only"));
  assert.match(markup, /Linux limited-network sockets/);
  assert.deepEqual(LINUX_SOCKETS.map((option) => option.value), ["tcp_only", "refuse", "tcp_ports"]);
  for (const option of LINUX_SOCKETS) {
    assert.ok(markup.includes(option.title.replaceAll("'", "&#x27;")), option.title);
    assert.ok(markup.includes(option.detail.replaceAll("'", "&#x27;")), option.detail);
  }
  assert.match(LINUX_SOCKETS[0].detail, /resolver ignores RES_OPTIONS/);
  assert.match(LINUX_SOCKETS[0].detail, /cannot look names up/);
  assert.match(LINUX_SOCKETS[0].detail, /Wrappers are judged by their first file, a script by its interpreter/);
  assert.match(LINUX_SOCKETS[1].detail, /master is unavailable/);
  assert.match(LINUX_SOCKETS[2].detail, /Hosts and UDP are not limited/);
});

test("the stored option is checked and Save changes waits for another pick", () => {
  for (const value of ["tcp_only", "refuse", "tcp_ports"]) {
    const markup = view(ok(value));
    assert.deepEqual(checked(markup), [value]);
    assert.ok(saveDisabled(markup), `${value} saves nothing until changed`);
  }
});

test("picking TCP only over another option enables Save changes", () => {
  const markup = view(ok("tcp_ports"), { picked: "tcp_only" });
  assert.deepEqual(checked(markup), ["tcp_only"]);
  assert.ok(!saveDisabled(markup));
});

test("picking Do not start limited-network agents enables Save changes", () => {
  const markup = view(ok("tcp_only"), { picked: "refuse" });
  assert.deepEqual(checked(markup), ["refuse"]);
  assert.ok(!saveDisabled(markup));
});

test("picking TCP ports only enables Save changes and a refused save shows why", () => {
  const markup = view(ok("tcp_only"), { picked: "tcp_ports", problem: "Another change is still running. Wait for it to finish." });
  assert.deepEqual(checked(markup), ["tcp_ports"]);
  assert.ok(!saveDisabled(markup));
  assert.match(markup, /role="alert"[^>]*>[\s\S]*Another change is still running/);
  const saving = view(ok("tcp_only"), { picked: "tcp_ports", saving: true });
  assert.match(saving, /<fieldset[^>]*disabled=""/);
  assert.match(saving, /Saving\.\.\./);
});

test("Settings, Sandbox lists the Linux call supervisor's On and Off with what each keeps, in the TUI's words (DEC-179 T4c)", () => {
  const markup = view(stored("tcp_only", "on"));
  assert.match(markup, /Linux call supervisor/);
  assert.deepEqual(LINUX_SUPERVISOR.map((option) => option.value), ["on", "off"]);
  assert.equal(LINUX_SUPERVISOR_DEFAULT, "on");
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/app_setup.rs", import.meta.url), "utf8");
  for (const option of LINUX_SUPERVISOR) {
    assert.ok(markup.includes(option.title.replaceAll("'", "&#x27;")), option.title);
    assert.ok(markup.includes(option.detail.replaceAll("'", "&#x27;")), option.detail);
    assert.ok(tui.includes(`"${option.title}"`), `the TUI says ${option.title}`);
    assert.ok(tui.includes(`"${option.detail}"`), `the TUI says ${option.detail}`);
  }
  assert.match(LINUX_SUPERVISOR[0].detail, /makes them for them/);
  assert.match(LINUX_SUPERVISOR[1].detail, /open to them on every host/);
});

test("each supervisor option is checked as stored, and picking the other enables Save with the sockets choice kept", () => {
  for (const [value, other] of [["on", "off"], ["off", "on"]]) {
    const markup = view(stored("tcp_ports", value));
    assert.deepEqual(supervised(markup), [value]);
    assert.deepEqual(checked(markup), ["tcp_ports"]);
    assert.ok(saveDisabled(markup), `${value} saves nothing until changed`);
    const picked = view(stored("tcp_ports", value), { pickedSupervisor: other });
    assert.deepEqual(supervised(picked), [other]);
    assert.deepEqual(checked(picked), ["tcp_ports"], "the sockets choice stays");
    assert.ok(!saveDisabled(picked));
  }
});

test("Save sends every Linux setting, each as picked or else as stored", () => {
  const all = { linux_sockets: "refuse", linux_supervisor: "on", linux_strict: "on" };
  assert.deepEqual(sandboxDraft(all, null, null, null), all);
  assert.deepEqual(sandboxDraft(all, null, "off", null), { ...all, linux_supervisor: "off" });
  assert.deepEqual(sandboxDraft(all, "tcp_only", null, null), { ...all, linux_sockets: "tcp_only" });
  assert.deepEqual(sandboxDraft(all, null, null, "off"), { ...all, linux_strict: "off" });
  assert.deepEqual(sandboxDraft(all, "tcp_ports", "off", "off"), { linux_sockets: "tcp_ports", linux_supervisor: "off", linux_strict: "off" });
  const source = readFileSync(new URL("../src/screens/settings/sandbox.tsx", import.meta.url), "utf8");
  assert.match(source, /saveSandboxSettings\(sandboxDraft\(settings\.value, picked, pickedSupervisor, pickedStrict\)\)/);
});

test("Settings, Sandbox lists Refuse agents without the full Linux sandbox, Off by default, in the TUI's words (DEC-185)", () => {
  const markup = view(stored("tcp_only", "on"));
  assert.match(markup, /Refuse agents without the full Linux sandbox/);
  assert.deepEqual(LINUX_STRICT.map((option) => option.value), ["off", "on"]);
  assert.equal(LINUX_STRICT_DEFAULT, "off");
  assert.deepEqual(strictly(markup), ["off"]);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/app_setup.rs", import.meta.url), "utf8");
  for (const option of LINUX_STRICT) {
    assert.ok(markup.includes(option.title.replaceAll("'", "&#x27;")), option.title);
    assert.ok(markup.includes(option.detail.replaceAll("'", "&#x27;")), option.detail);
    assert.ok(tui.includes(`"${option.title}"`), `the TUI says ${option.title}`);
    assert.ok(tui.includes(`"${option.detail}"`), `the TUI says ${option.detail}`);
  }
  assert.match(LINUX_STRICT[1].detail, /process space of its own/);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox.rs", import.meta.url), "utf8");
  const body = engine.match(/#\[serde\(rename_all = "snake_case"\)\]\s*pub enum LinuxStrict \{([^}]*)\}/);
  assert.ok(body, "sandbox.rs declares LinuxStrict in snake case");
  const names = [...body[1].matchAll(/^\s*([A-Z][A-Za-z]*),/gm)].map(([, name]) => name.toLowerCase());
  assert.deepEqual(names, LINUX_STRICT.map((option) => option.value));
  assert.match(engine, /pub enum LinuxStrict \{\s*#\[default\]\s*Off,/, "Off is the engine's default");
});

test("each strict option is checked as stored, and picking the other enables Save with the other choices kept", () => {
  for (const [value, other] of [["off", "on"], ["on", "off"]]) {
    const markup = view(stored("tcp_ports", "off", value));
    assert.deepEqual(strictly(markup), [value]);
    assert.ok(saveDisabled(markup), `${value} saves nothing until changed`);
    const picked = view(stored("tcp_ports", "off", value), { pickedStrict: other });
    assert.deepEqual(strictly(picked), [other]);
    assert.deepEqual(checked(picked), ["tcp_ports"], "the sockets choice stays");
    assert.deepEqual(supervised(picked), ["off"], "the supervisor choice stays");
    assert.ok(!saveDisabled(picked));
  }
});

test("the supervisor options are the engine's LinuxSupervisor names", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox.rs", import.meta.url), "utf8");
  const body = engine.match(/#\[serde\(rename_all = "snake_case"\)\]\s*pub enum LinuxSupervisor \{([^}]*)\}/);
  assert.ok(body, "sandbox.rs declares LinuxSupervisor in snake case");
  const names = [...body[1].matchAll(/^\s*([A-Z][A-Za-z]*),/gm)].map(([, name]) => name.toLowerCase());
  assert.deepEqual(names, LINUX_SUPERVISOR.map((option) => option.value));
  assert.match(engine, /pub enum LinuxSupervisor \{\s*#\[default\]\s*On,/, "On is the engine's default");
});

test("loading, not ready and a failed read each say so", () => {
  assert.doesNotMatch(view({ kind: "loading" }), /linux-sockets/);
  assert.match(view({ kind: "not_ready" }), /sandbox settings/i);
  assert.match(view({ kind: "error", code: "owner_read_failed", message: "The engine is busy." }), /Couldn&#x27;t load the sandbox settings\. The engine is busy\./);
});

test("each Linux launch refusal reads plainly and names the setting", () => {
  assert.match(knownError("linux_resolver_needs_udp"), /cannot look up names over TCP[\s\S]*'TCP only'[\s\S]*'TCP ports only'[\s\S]*UDP and other sockets reach any host/);
  assert.match(knownError("linux_harness_unchecked"), /cannot check[\s\S]*'TCP only'[\s\S]*does not start[\s\S]*'TCP ports only'[\s\S]*reach any host/);
  assert.match(knownError("linux_limited_network_off"), /Linux limited-network sockets[\s\S]*does not start/);
  assert.match(knownError("linux_sandbox_not_full"), /'Refuse agents without the full Linux sandbox'[\s\S]*Agent access[\s\S]*Settings/);
});

test("the options are the engine's LinuxSockets names", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox.rs", import.meta.url), "utf8");
  const body = engine.match(/#\[serde\(rename_all = "snake_case"\)\]\s*pub enum LinuxSockets \{([^}]*)\}/);
  assert.ok(body, "sandbox.rs declares LinuxSockets in snake case");
  const names = [...body[1].matchAll(/^\s*([A-Z][A-Za-z]*),/gm)]
    .map(([, name]) => name.replace(/(?<=[a-z])([A-Z])/g, "_$1").toLowerCase());
  assert.deepEqual([...names].sort(), LINUX_SOCKETS.map((option) => option.value).sort());
});
