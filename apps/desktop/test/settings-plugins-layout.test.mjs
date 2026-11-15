import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { CONTRAST_MIN, doublePress, press, tabRunner, tabWalk } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-plugins-layout.mjs", import.meta.url).pathname);
const onPlugins = tabRunner(fixture);
const RADIO = "[role=radio]";
const read = (page) => page.evaluate("Fixture.sections()");
const say = (page) => page.evaluate("Fixture.text()");
const sent = (page) => page.evaluate("Fixture.commands()");
const SECTIONS = [
  ["Skills", "skills", "Couldn't load skills.", "Skills isn't ready yet", "No skills yet", ["Import my skills", "Add skill"]],
  ["MCP servers", "servers", "Couldn't load MCP servers.", "MCP servers isn't ready yet", "No MCP servers yet", ["Add MCP server"]],
  ["Hooks", "hooks", "Couldn't load hooks.", "Hooks isn't ready yet", "No hooks yet", ["Add hooks from a plugin"]],
  ["Tools", "tools", "Couldn't load the tools.", "Tool switches isn't ready yet", "No tools yet. Check the server to list its tools.", []],
];

/** Opens one section with a real click. Inputs: the page and the section's name. */
const openSection = (page, name) => press(page, RADIO, name);

test("the top shows the title and the four sections, then Skills with its purpose and actions in its head", async () => {
  const seen = await onPlugins({}, async (page) => ({ top: await page.evaluate("Fixture.top()"), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Plugins and skills", JSON.stringify(seen.top));
  assert.deepEqual(seen.top.actions, ["Skills", "MCP servers", "Hooks", "Tools"]);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Skills"]);
  assert.match(seen.sections[0].about, /A skill is a folder with a SKILL\.md file\./);
  assert.deepEqual(seen.sections[0].actions, ["Import my skills", "Add skill"]);
});

test("a skill line keeps its name link, description and folder left, and its switch, Open and Remove right", async () => {
  const [section] = await onPlugins({}, read);
  const [first, refused] = section.rows;
  assert.equal(first.label, "frontend-design");
  for (const part of ["Builds polished front ends.", "~/skills/frontend-design"]) assert.ok(first.help.includes(part), `${part} in ${first.help}`);
  assert.ok(first.controlLeft > first.labelRight, JSON.stringify(first));
  assert.deepEqual(first.buttons, ["On for every project", "Open frontend-design", "Remove frontend-design"]);
  assert.deepEqual(first.visible, ["", "Open", "Remove"]);
  assert.ok(refused.help.includes("Not given to agents"), refused.help);
  assert.ok(refused.help.includes("Agents don't get this skill."), refused.help);
});

test("real Tab keys walk the section picker, the head actions, then each line's controls", async () => {
  const walked = await onPlugins({}, (page) => tabWalk(page, 8));
  const from = walked.indexOf("Import my skills");
  assert.notEqual(from, -1, JSON.stringify(walked));
  assert.equal(walked[from - 1], "Skills", JSON.stringify(walked));
  assert.deepEqual(walked.slice(from, from + 6), ["Import my skills", "Add skill", "Skill frontend-design: Builds polished front ends.", "On for every project", "Open frontend-design", "Remove frontend-design"]);
});

for (const [name, key, failed, missing, empty, actions] of SECTIONS) {
  test(`${name} keeps its error, not ready and empty states`, async () => {
    const shown = async (state) => onPlugins({ [key]: state }, async (page) => { await openSection(page, name); return say(page); });
    assert.ok((await shown("error")).includes(failed), failed);
    assert.ok((await shown("missing")).includes(missing), missing);
    const none = await shown("empty");
    assert.ok(none.includes(empty), none);
    for (const action of actions) assert.ok(none.includes(action), `${action} in ${none}`);
  });
}

test("each section draws its skeletons while it reads", async () => {
  for (const [name, key] of SECTIONS) {
    const skeletons = await onPlugins({ [key]: "pending" }, async (page) => { await openSection(page, name); return page.evaluate("Fixture.count('.skeleton')"); });
    assert.ok(skeletons >= 3, `${name}: ${skeletons}`);
  }
});

test("a server line shows its status with its name, what it runs and its controls", async () => {
  const [section] = await onPlugins({}, async (page) => { await openSection(page, "MCP servers"); return read(page); });
  assert.deepEqual([section.heading, section.actions], ["MCP servers", ["Add MCP server"]]);
  const [approved, waiting, broken, remote] = section.rows;
  assert.equal(approved.label, "github");
  assert.ok(approved.words.includes("Approved"), approved.words);
  assert.ok(approved.help.includes("/bin/npx -y github"), approved.help);
  assert.deepEqual(approved.visible, ["", "Check", "Secrets", "Remove"]);
  assert.deepEqual(approved.buttons, ["On for every project", "Check github", "Secrets for github", "Remove github"]);
  assert.ok(waiting.words.includes("Needs approval"), waiting.words);
  assert.ok(waiting.help.includes("Runs: /bin/wait now"), waiting.help);
  assert.ok(waiting.help.includes("Secrets: GITHUB_TOKEN (Key saved)"), waiting.help);
  assert.deepEqual(waiting.visible, ["", "Check", "Approve", "Secrets", "Remove"]);
  assert.ok(broken.help.includes("Can't start: the server exited."), broken.help);
  assert.deepEqual(broken.visible, ["", "Check again", "Secrets", "Remove"]);
  assert.ok(remote.help.includes("Connects to: https://mcp.example.invalid/sse"), remote.help);
  assert.deepEqual(remote.visible, ["", "Approve", "Secrets", "Remove"]);
});

test("N tools opens the server's own tool switches under its line and says it is open", async () => {
  const seen = await onPlugins({}, async (page) => {
    await openSection(page, "MCP servers");
    const before = await page.evaluate("Fixture.count('.review .set-row')");
    await press(page, ".set-help button", "2 tools");
    return { before, after: await page.evaluate("Fixture.count('.review .set-row')"), expanded: await page.evaluate("document.querySelector('.set-help button').getAttribute('aria-expanded')"), words: await page.evaluate("Fixture.wordsOf('.review .set-row .set-label')"), deletes: await page.evaluate("Fixture.count('.review button[aria-label^=Delete]')"), border: await page.evaluate("getComputedStyle(document.querySelector('.review .set-row')).borderTopWidth") };
  });
  assert.deepEqual([seen.before, seen.after, seen.expanded], [0, 1, "true"]);
  assert.equal(seen.words, "echo");
  assert.equal(seen.deletes, 0, "a server's own tools can't be deleted");
  assert.equal(seen.border, "0px", "the box's first line has no rule above it");
});

for (const [action, selector, command] of [
  ["Check", 'button[aria-label="Check github"]', "desktop_check_mcp_server"],
  ["Approve", 'button[aria-label="Approve waiting"]', "desktop_approve_mcp_server"],
]) {
  test(`${action} on a server line reaches the engine and a refusal shows in the section`, async () => {
    const seen = await onPlugins({}, async (page) => {
      await openSection(page, "MCP servers");
      await press(page, selector);
      return { commands: await sent(page), alert: await page.evaluate("Fixture.alert()") };
    });
    assert.ok(seen.commands.includes(command), JSON.stringify(seen.commands));
    assert.match(seen.alert.words, /engine_failed/);
  });
}

for (const [section, label, title, command] of [
  ["Skills", "Remove frontend-design", "Remove this skill?", "desktop_remove_skill"],
  ["MCP servers", "Remove github", "Remove this MCP server?", "desktop_remove_mcp_server"],
]) {
  test(`${label} asks first, then reaches the engine`, async () => {
    const seen = await onPlugins({}, async (page) => {
      await openSection(page, section);
      await press(page, `button[aria-label="${label}"]`);
      const asking = await page.evaluate("document.querySelector('dialog[open]')?.innerText ?? ''");
      const before = await sent(page);
      await press(page, "dialog[open] button", "Remove");
      return { asking, before, after: await sent(page) };
    });
    assert.ok(seen.asking.includes(title), seen.asking);
    assert.ok(!seen.before.includes(command), JSON.stringify(seen.before));
    assert.ok(seen.after.includes(command), JSON.stringify(seen.after));
  });
}

for (const [section, button, title] of [
  ["Skills", "Import my skills", "Import my skills"],
  ["MCP servers", "Add MCP server", "Add MCP server"],
  ["Tools", "Add a tool", "Add MCP server"],
]) {
  test(`${button} opens its dialog`, async () => {
    const heading = await onPlugins({}, async (page) => {
      await openSection(page, section);
      await press(page, ".set-head button", button);
      return page.evaluate("document.querySelector('dialog[open] h2')?.innerText ?? ''");
    });
    assert.equal(heading, title);
  });
}

test("Secrets opens the server's dialog and Open goes to the skill's route", async () => {
  const dialog = await onPlugins({}, async (page) => {
    await openSection(page, "MCP servers");
    await press(page, 'button[aria-label="Secrets for github"]');
    return page.evaluate("document.querySelector('dialog[open] h2')?.innerText ?? ''");
  });
  assert.equal(dialog, "Secrets for github");
  const hash = await onPlugins({}, async (page) => {
    await press(page, 'button[aria-label="Open frontend-design"]');
    return page.evaluate("location.hash");
  });
  assert.match(hash, /#\/settings\/plugins\?skill=skill-1/);
});

test("a hook plugin line keeps its events count and what it follows, with its switch, and Show events opens its trigger and command", async () => {
  const seen = await onPlugins({}, async (page) => {
    await openSection(page, "Hooks");
    const folded = await read(page);
    await press(page, 'button[aria-label="Show caveman events"]');
    return { folded, opened: await read(page) };
  });
  const [section] = seen.folded;
  assert.deepEqual([section.heading, section.actions], ["Hooks", ["Add hooks from a plugin"]]);
  const [first, second] = section.rows;
  assert.equal(first.label, "caveman - 1 event");
  assert.ok(first.help.includes("Runs while the caveman skill is on."), first.help);
  assert.ok(!first.help.includes("node hook.js"), first.help);
  assert.deepEqual(first.buttons, ["On for every project"]);
  assert.equal(second.label, "adhd - 1 event");
  assert.ok(!second.help.includes("Runs while"), second.help);
  const event = seen.opened[0].rows[1];
  assert.equal(event.label, "SessionStart, matching startup");
  assert.ok(event.help.includes("node hook.js"), event.help);
  assert.deepEqual(event.buttons, ["On for every project"]);
});

test("Tools keeps its section, one switch per tool, Delete on built-in tools and Restore defaults", async () => {
  const seen = await onPlugins({}, async (page) => { await openSection(page, "Tools"); return { sections: await read(page), says: await say(page) }; });
  const [section] = seen.sections;
  assert.equal(section.heading, "CrazyAgent tools");
  assert.deepEqual(section.actions, ["Add a tool"]);
  assert.match(section.about, /Delete hides a built-in tool until you restore the defaults\./);
  assert.deepEqual(section.rows.map((row) => row.label), ["start_work", "graphify"]);
  assert.deepEqual(section.rows[0].buttons, ["On for every project", "Delete start_work"]);
  for (const part of ["1 deleted. Restore defaults brings it back.", "Restore defaults", "Each MCP server's tools have their own switches"]) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
});

test("Delete on a tool reaches the engine and a refusal shows in the section", async () => {
  const seen = await onPlugins({}, async (page) => {
    await openSection(page, "Tools");
    await press(page, 'button[aria-label="Delete start_work"]');
    return { commands: await sent(page), alert: await page.evaluate("Fixture.alert()") };
  });
  assert.ok(seen.commands.includes("desktop_set_tool_switch"), JSON.stringify(seen.commands));
  assert.match(seen.alert.words, /engine_failed/);
});

/** Reads one button by its name. Inputs: the page and the name. Output: its visible words and
 * whether it is disabled. */
const buttonState = (page, name) => page.evaluate(`(() => {
  const found = document.querySelector('button[aria-label="${name}"]');
  return { words: found?.textContent, disabled: found?.disabled };
})()`);

test("a double press on Check runs one check and shows it is working", async () => {
  const seen = await onPlugins({ hold: "desktop_check_mcp_server" }, async (page) => {
    await openSection(page, "MCP servers");
    await doublePress(page, 'button[aria-label="Check github"]');
    return { commands: await sent(page), button: await buttonState(page, "Check github") };
  });
  assert.equal(seen.commands.filter((command) => command === "desktop_check_mcp_server").length, 1, JSON.stringify(seen.commands));
  assert.deepEqual(seen.button, { words: "Checking...", disabled: true });
});

test("while a waiting server is checked, Approve waits and keeps its own word", async () => {
  const seen = await onPlugins({ hold: "desktop_check_mcp_server" }, async (page) => {
    await openSection(page, "MCP servers");
    await press(page, 'button[aria-label="Check waiting"]');
    return { check: await buttonState(page, "Check waiting"), approve: await buttonState(page, "Approve waiting") };
  });
  assert.deepEqual(seen.check, { words: "Checking...", disabled: true });
  assert.deepEqual(seen.approve, { words: "Approve", disabled: true });
});

test("while a server is approved, its Check waits and keeps its own word", async () => {
  const seen = await onPlugins({ hold: "desktop_approve_mcp_server" }, async (page) => {
    await openSection(page, "MCP servers");
    await press(page, 'button[aria-label="Approve waiting"]');
    return { check: await buttonState(page, "Check waiting"), approve: await buttonState(page, "Approve waiting") };
  });
  assert.deepEqual(seen.approve, { words: "Approving...", disabled: true });
  assert.deepEqual(seen.check, { words: "Check", disabled: true });
});

test("a failed Check lets go of its button, with its word back", async () => {
  const button = await onPlugins({}, async (page) => {
    await openSection(page, "MCP servers");
    await press(page, 'button[aria-label="Check github"]');
    return buttonState(page, "Check github");
  });
  assert.deepEqual(button, { words: "Check", disabled: false });
});

test("while one server is checked, every other server's Check and Approve wait, and no second call goes out", async () => {
  const seen = await onPlugins({ hold: "desktop_check_mcp_server" }, async (page) => {
    await openSection(page, "MCP servers");
    await press(page, 'button[aria-label="Check github"]');
    await press(page, 'button[aria-label="Check waiting"]');
    return {
      first: await buttonState(page, "Check github"), check: await buttonState(page, "Check waiting"), approve: await buttonState(page, "Approve waiting"),
      again: await buttonState(page, "Check again broken"), commands: await sent(page),
    };
  });
  assert.deepEqual(seen.first, { words: "Checking...", disabled: true });
  assert.deepEqual(seen.check, { words: "Check", disabled: true });
  assert.deepEqual(seen.approve, { words: "Approve", disabled: true });
  assert.deepEqual(seen.again, { words: "Check again", disabled: true });
  assert.equal(seen.commands.filter((command) => command === "desktop_check_mcp_server").length, 1, JSON.stringify(seen.commands));
});

test("a broken server's Check again is named with the words it shows", async () => {
  const [section] = await onPlugins({}, async (page) => { await openSection(page, "MCP servers"); return read(page); });
  const broken = section.rows.find((row) => row.label === "broken");
  assert.equal(broken.buttons.includes("Check again broken"), true, JSON.stringify(broken.buttons));
  assert.equal(broken.visible.includes("Check again"), true, JSON.stringify(broken.visible));
  assert.equal(section.rows.find((row) => row.label === "github").buttons.includes("Check github"), true);
});

for (const [name, key] of [["Skills", "skills"], ["MCP servers", "servers"], ["Hooks", "hooks"], ["Tools", "tools"]]) {
  test(`in a project a ${name} line with its own switch is marked changed and the next line is not`, async () => {
    const [section] = await onPlugins({ project: "app", [key]: "here" }, async (page) => { await openSection(page, name); return read(page); });
    assert.deepEqual(section.rows.map((row) => row.mark), ["changed", null]);
  });
}

for (const [name, key] of [["Skills", "skills"], ["MCP servers", "servers"], ["Hooks", "hooks"]]) {
  test(`on the every-project page a ${name} line's project switch is not marked`, async () => {
    const [section] = await onPlugins({ [key]: "here" }, async (page) => { await openSection(page, name); return read(page); });
    assert.deepEqual(section.rows.map((row) => row.mark), [null, null]);
  });
}

test("on the every-project page a tool line is marked changed only when its switch is not on", async () => {
  const [section] = await onPlugins({}, async (page) => { await openSection(page, "Tools"); return read(page); });
  assert.deepEqual(section.rows.map((row) => [row.label, row.mark]), [["start_work", null], ["graphify", "changed"]]);
});

test("a project shows no title, no Import my skills and no Remove, with its own switch words", async () => {
  const seen = await onPlugins({ project: "app" }, async (page) => {
    const skills = await read(page);
    await openSection(page, "MCP servers");
    return { top: await page.evaluate("Fixture.top()"), skills, servers: await read(page), radios: await page.evaluate("Fixture.count('[role=radio]')") };
  });
  assert.equal(seen.top, null);
  assert.equal(seen.radios, 4);
  assert.deepEqual(seen.skills[0].actions, ["Add skill"]);
  assert.deepEqual(seen.skills[0].rows[0].buttons, ["On in this project", "Open frontend-design"]);
  assert.deepEqual(seen.servers[0].rows[0].buttons, ["On in this project", "Check github"]);
});

for (const theme of ["light", "dark"]) {
  test(`a line's help and its mono path read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onPlugins({}, async (page) => ({ help: await page.evaluate('Fixture.contrast(".set-help")'), path: await page.evaluate('Fixture.contrast(".set-help .mono")') }), 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
