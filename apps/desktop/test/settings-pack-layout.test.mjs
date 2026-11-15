import assert from "node:assert/strict";
import test from "node:test";
import { CONTRAST_MIN, argsOf, commands, contrasts, count, dialog, doublePress, onTab, press, read, say, tabStops, toasts, top } from "./settings-agents-page.mjs";

const PURPOSE = "The skills and plugins every project starts with. Switching one off stops agents using it; removing it deletes what it installed.";
const GRAPHIFY = "Graphify skill for agents";
const on = (scene, check, width, theme) => onTab("pack", scene, check, width, theme);
const row = (name) => `[data-setting="${name}"]`;

function section(sections, heading) {
  const found = sections.find((item) => item.heading === heading);
  assert.ok(found, `${heading} in ${JSON.stringify(sections.map((item) => item.heading))}`);
  return found;
}

function entry(sections, heading, name) {
  const found = section(sections, heading).rows.find((item) => item.label.includes(name));
  assert.ok(found, `${name} in ${heading}`);
  return found;
}

test("the tab shows its title and purpose, then one section per pack group, then Tools and Unused tools", async () => {
  const seen = await on({}, async (page) => ({ top: await top(page), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Pack", JSON.stringify(seen.top));
  assert.equal(seen.top.purpose, PURPOSE);
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((item) => item.heading), ["Code graph", "Workflow skills", "Design skills", "Reply style", "Context plugins", "Optional", "Other", "Tools", "Unused tools"]);
  assert.equal(section(seen.sections, "Workflow skills").about, "How agents plan, build, test and review.");
  assert.equal(section(seen.sections, "Other").about, "");
  assert.equal(section(seen.sections, "Tools").about, "tmux, uv and graphify are installed.");
});

test("a pack group is marked changed only when an installed entry differs from its shipped default", async () => {
  const sections = await on({}, read);
  assert.deepEqual(sections.map((item) => [item.heading, item.mark]), [["Code graph", null], ["Workflow skills", "changed"], ["Design skills", null], ["Reply style", null], ["Context plugins", null], ["Optional", null], ["Other", "changed"], ["Tools", null], ["Unused tools", null]]);
});

test("an entry has its name and state on the left, its source under it and its controls on the right", async () => {
  const sections = await on({}, read);
  const graphify = entry(sections, "Code graph", GRAPHIFY);
  assert.ok(graphify.label.includes("On"), graphify.label);
  assert.ok(graphify.help.includes("github.com/example/graphify, MIT"), graphify.help);
  assert.ok(graphify.controlLeft > graphify.labelRight, JSON.stringify(graphify));
  assert.deepEqual(graphify.buttons, [`Use ${GRAPHIFY}`, "Remove"]);
});

test("each entry state keeps its word, its detail line and its own controls", async () => {
  const seen = await on({}, async (page) => ({ sections: await read(page), links: await count(page, 'a.skill-tag[href*="skill-superpowers"]') }));
  const off = entry(seen.sections, "Workflow skills", "superpowers");
  assert.ok(off.label.includes("Off"), off.label);
  assert.ok(off.help.includes("Hooks not added: the hook is outside its folder"), off.help);
  assert.deepEqual(off.buttons, ["Use superpowers", "Remove"]);
  assert.equal(seen.links, 1, "a listed skill links to its page");
  const available = entry(seen.sections, "Design skills", "taste");
  assert.ok(available.label.includes("Not installed"), available.label);
  assert.deepEqual(available.buttons, ["Install"]);
  const failed = entry(seen.sections, "Reply style", "caveman");
  assert.ok(failed.label.includes("Failed"), failed.label);
  assert.ok(failed.help.includes("the download failed"), failed.help);
  assert.deepEqual(failed.buttons, ["Try again"]);
  assert.ok(entry(seen.sections, "Context plugins", "context-mode").label.includes("MCP server"));
});

test("a tool line has its name on the left, what it is for under it and its state on the right", async () => {
  const sections = await on({}, read);
  const tools = section(sections, "Tools");
  assert.deepEqual(tools.actions, [], "nothing to install");
  const [tmux, uv, graphify] = tools.rows;
  assert.deepEqual([tmux.label, tmux.control], ["tmux", "Found"]);
  assert.ok(tmux.controlLeft > tmux.labelRight, JSON.stringify(tmux));
  for (const part of ["Runs each agent in a live terminal.", "Version 3.5a on this Mac."]) assert.ok(tmux.help.includes(part), `${part} in ${tmux.help}`);
  assert.deepEqual([uv.label, uv.control], ["uv", "Installed"]);
  assert.deepEqual([graphify.label, graphify.control], ["Graphify command", "Found"]);
});

test("a missing or failed tool offers Install tools, keeps its reason and the readiness note", async () => {
  const seen = await on({ setup: "wanted" }, async (page) => {
    const sections = await read(page);
    const says = await say(page);
    await press(page, ".set-head button", "Install tools");
    return { sections, says, runs: await argsOf(page, "desktop_run_setup"), toasts: await toasts(page) };
  });
  const tools = section(seen.sections, "Tools");
  assert.deepEqual(tools.actions, ["Install tools"]);
  assert.equal(tools.rows[1].control, "Not installed");
  assert.ok(tools.rows[1].help.includes("Setup installs it."), tools.rows[1].help);
  assert.equal(tools.rows[2].control, "Failed");
  assert.ok(tools.rows[2].help.includes("uv could not fetch it"), tools.rows[2].help);
  assert.ok(seen.says.includes("Agent git needs the Command Line Tools."), seen.says);
  assert.deepEqual(seen.runs.map((args) => args.pack), [[]]);
  assert.deepEqual(seen.toasts, ["Installing the missing tools."]);
});

test("a double press on Install tools starts setup once", async () => {
  const runs = await on({ setup: "wanted", hold: "desktop_run_setup" }, async (page) => {
    await doublePress(page, ".set-head button", "Install tools");
    return argsOf(page, "desktop_run_setup");
  });
  assert.equal(runs.length, 1, JSON.stringify(runs));
});

test("no tool read leaves the Tools section out", async () => {
  const sections = await on({ setup: "bare" }, read);
  assert.ok(!sections.some((item) => item.heading === "Tools"), JSON.stringify(sections.map((item) => item.heading)));
});

test("while setup installs every change waits and the installing entry has no button", async () => {
  const seen = await on({ setup: "running" }, async (page) => ({
    says: await say(page), sections: await read(page), switches: await count(page, 'button[role="switch"][aria-disabled="true"]'), disabled: await count(page, ".set-row button:disabled"),
  }));
  assert.ok(seen.says.includes("Setup is installing. Changes wait until it finishes."), seen.says);
  assert.ok(seen.says.includes("Wait for the current change."), seen.says);
  assert.equal(seen.switches, 4);
  assert.equal(seen.disabled, 6);
  const installing = entry(seen.sections, "Optional", "headroom");
  assert.ok(installing.label.includes("Installing"), installing.label);
  assert.deepEqual(installing.buttons, []);
});

test("while the pack loads the title stays above the skeletons", async () => {
  const seen = await on({ setup: "pending" }, async (page) => ({ top: await top(page), skeletons: await count(page, ".skeleton") }));
  assert.equal(seen.top?.heading, "Pack", JSON.stringify(seen.top));
  assert.equal(seen.skeletons, 6);
});

for (const [state, expected] of [["missing", "The default pack isn't ready yet"], ["error", "Try again"]]) {
  test(`the ${state} state stands under the title`, async () => {
    const seen = await on({ setup: state }, async (page) => ({ top: await top(page), says: await say(page), sections: await read(page) }));
    assert.equal(seen.top?.heading, "Pack", JSON.stringify(seen.top));
    assert.deepEqual(seen.sections, []);
    assert.ok(seen.says.includes(expected), seen.says);
  });
}

test("Try again reads the pack a second time", async () => {
  const reads = await on({ setup: "error" }, async (page) => {
    await press(page, "button", "Try again");
    return (await commands(page)).filter((command) => command === "desktop_setup_status");
  });
  assert.equal(reads.length, 2);
});

test("a real press on a switch saves it and says so", async () => {
  const seen = await on({}, async (page) => {
    await press(page, "#pack-graphify");
    return { sent: await argsOf(page, "desktop_set_pack_entry"), toasts: await toasts(page) };
  });
  assert.deepEqual(seen.sent.map(({ id, on: wanted, version }) => [id, wanted, version]), [["graphify", false, 7]]);
  assert.deepEqual(seen.toasts, [`${GRAPHIFY} is off.`]);
});

test("a change this build lacks shows its alert under the title", async () => {
  const alert = await on({ absent: "desktop_set_pack_entry" }, async (page) => {
    await press(page, "#pack-graphify");
    return page.evaluate("Fixture.alert()");
  });
  assert.match(alert.words, /Changing the pack isn't in this build\./);
  assert.equal(alert.belowTop, true, JSON.stringify(alert));
});

test("Remove asks first and sends only on the confirm", async () => {
  const seen = await on({}, async (page) => {
    await press(page, `${row(GRAPHIFY)} button`, "Remove");
    const asking = await dialog(page);
    const before = await argsOf(page, "desktop_remove_pack_entry");
    await press(page, "dialog[open] button", "Remove");
    return { asking, before, sent: await argsOf(page, "desktop_remove_pack_entry"), toasts: await toasts(page) };
  });
  for (const part of [`Remove ${GRAPHIFY}?`, "CrazyAgent deletes the skill it installed for every project. You can install it again from this page.", "Cancel", "Remove"]) assert.ok(seen.asking.includes(part), `${part} in ${seen.asking}`);
  assert.deepEqual(seen.before, []);
  assert.deepEqual(seen.sent.map(({ id, version }) => [id, version]), [["graphify", 7]]);
  assert.deepEqual(seen.toasts, [`${GRAPHIFY} was removed.`]);
});

test("Cancel closes the remove dialog and sends nothing, and an MCP server is named as one", async () => {
  const seen = await on({}, async (page) => {
    await press(page, `${row("context-mode")} button`, "Remove");
    const asking = await dialog(page);
    await press(page, "dialog[open] button", "Cancel");
    return { asking, open: await count(page, "dialog[open]"), sent: await argsOf(page, "desktop_remove_pack_entry") };
  });
  assert.ok(seen.asking.includes("CrazyAgent deletes the MCP server it installed for every project."), seen.asking);
  assert.equal(seen.open, 0);
  assert.deepEqual(seen.sent, []);
});

test("Install asks first, names what it lets agents run and starts on the confirm", async () => {
  const seen = await on({}, async (page) => {
    await press(page, `${row("taste")} button`, "Install");
    const skill = await dialog(page);
    await press(page, "dialog[open] button", "Cancel");
    await press(page, `${row("headroom")} button`, "Install");
    const server = await dialog(page);
    await press(page, "dialog[open] button", "Install");
    return { skill, server, runs: await argsOf(page, "desktop_run_setup"), toasts: await toasts(page) };
  });
  for (const part of ["Install taste?", "Agents in every project can use this skill. It comes from github.com/example/taste.", "Cancel", "Install"]) assert.ok(seen.skill.includes(part), `${part} in ${seen.skill}`);
  for (const part of ["Install headroom?", "Installing it lets your agents run this MCP server in every project. It comes from github.com/example/headroom."]) assert.ok(seen.server.includes(part), `${part} in ${seen.server}`);
  assert.deepEqual(seen.runs.map((args) => args.pack), [["headroom"]]);
  assert.deepEqual(seen.toasts, ["Installing headroom."]);
});

test("real Tab keys walk each entry's controls in group order", async () => {
  const walked = await on({}, (page) => tabStops(page, 12));
  assert.deepEqual(walked, [`Use ${GRAPHIFY}`, "Remove", "Skill superpowers: How agents plan and build.", "Use superpowers", "Remove", "Install", "Try again", "Use context-mode", "Remove", "Install", "Use oddity", "Remove"]);
});

for (const theme of ["light", "dark"]) {
  test(`the purpose, help, failure line and changed mark read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await on({}, (page) => contrasts(page, [".settings-top p", ".set-head p", ".set-help", ".set-help .pj-error", ".set-changed"]), 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
