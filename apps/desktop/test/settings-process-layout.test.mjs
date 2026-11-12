import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { CONTRAST_MIN, press, tabRunner, tabWalk } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-process-layout.mjs", import.meta.url).pathname);
const onProcess = tabRunner(fixture);
const FIELD = "#process-app";
const DIALOG = "dialog[open]";
const SAVE = "form button[type=submit]";
const read = (page) => page.evaluate("Fixture.sections()");
const say = (page) => page.evaluate("Fixture.text()");
const sent = (page) => page.evaluate("Fixture.commands()");
const top = (page) => page.evaluate("Fixture.top()");
const value = (page) => page.evaluate(`document.querySelector("${FIELD}").value`);
const saveOff = (page) => page.evaluate(`document.querySelector("${SAVE}").disabled`);
const dialog = (page) => page.evaluate(`Fixture.wordsOf("${DIALOG}")`);
const open = (page) => page.evaluate(`Fixture.count("${DIALOG}")`);
const button = (page, words) => press(page, "button", words);
const inDialog = (page, words) => press(page, `${DIALOG} button`, words);

/** Types into the instructions with real input. Inputs: the page and the words. */
async function typed(page, words) {
  await press(page, FIELD);
  await page.send("Input.insertText", { text: words });
  await page.evaluate("Fixture.settle(80)");
}

test("the top shows Process and the app's purpose line, with no action", async () => {
  const seen = await onProcess({}, top);
  assert.equal(seen?.heading, "Process", JSON.stringify(seen));
  assert.match(seen.purpose, /Instructions every master and sub-agent gets, in every project\. A project's own instructions come after these\./);
  assert.deepEqual(seen.actions, []);
});

test("four sections in order, each with the purpose line it has", async () => {
  const sections = await onProcess({}, read);
  assert.deepEqual(sections.map((section) => section.heading), ["Start from", "Instructions for agents", "Edit with AI", "As agents read it"]);
  assert.equal(sections[0].about, "Both open a preview first. Nothing changes until you save.");
  assert.equal(sections[1].about, "");
  assert.equal(sections[2].about, "A model proposes a cleaned file. You review every change before anything is saved.");
  assert.equal(sections[3].about, "Highlighted names are skills. Each opens its own details.");
});

test("the app-wide Edit with AI switch: On shows the button and asks for a pool on every click, Off hides it, and a click on the switch saves the choice", async () => {
  await onProcess({}, async (page) => {
    const state = () => page.evaluate(`({
      button: [...document.querySelectorAll("button")].some((node) => node.textContent.trim() === "Edit with AI"),
      on: document.getElementById("process-edit-with-ai")?.getAttribute("aria-checked") ?? null,
      asks: document.body.innerText.includes("Which pool runs this edit?"),
      picked: [...document.querySelectorAll('input[name="pr-ai-pool"]')].filter((node) => node.checked).length,
    })`);
    assert.deepEqual(await state(), { button: true, on: "true", asks: false, picked: 0 });
    for (const round of [1, 2]) {
      await button(page, "Edit with AI");
      await page.evaluate("new Promise((done) => setTimeout(done, 50))");
      assert.deepEqual(await state(), { button: true, on: "true", asks: true, picked: 0 }, `click ${round} asks for the pool with none picked`);
      assert.ok(!(await sent(page)).includes("desktop_process_propose"), "nothing is sent before a pool is picked");
      await button(page, "Cancel");
    }
    assert.ok(!(await sent(page)).includes("desktop_save_app_instruction_settings"));
    await page.evaluate('document.getElementById("process-edit-with-ai").click()');
    await page.evaluate("new Promise((done) => setTimeout(done, 50))");
    assert.ok((await sent(page)).includes("desktop_save_app_instruction_settings"), "the switch saves at once");
  });
  await onProcess({ ai: "off" }, async (page) => {
    const shown = await page.evaluate(`({
      button: [...document.querySelectorAll("button")].some((node) => node.textContent.trim() === "Edit with AI"),
      on: document.getElementById("process-edit-with-ai")?.getAttribute("aria-checked") ?? null,
    })`);
    assert.deepEqual(shown, { button: false, on: "false" }, "Off hides the button and keeps the switch");
  });
});

test("Start from has one line per seed, its words left and its button right", async () => {
  const [start] = await onProcess({}, read);
  const [file, shipped] = start.rows;
  assert.equal(start.rows.length, 2);
  assert.equal(file.label, "Your own file");
  assert.equal(file.help, "Read a text file into the editor.");
  assert.deepEqual(file.buttons, ["Import from file"]);
  assert.ok(file.controlLeft > file.labelRight, JSON.stringify(file));
  assert.equal(shipped.label, "CrazyAgent default");
  assert.equal(shipped.help, "The process CrazyAgent ships with.");
  assert.deepEqual(shipped.buttons, ["Use the CrazyAgent default process"]);
  assert.ok(shipped.controlLeft > shipped.labelRight, JSON.stringify(shipped));
});

test("at a narrow width the long seed button wraps inside its column and never crosses the label", async () => {
  const [start] = await onProcess({}, read, 640);
  for (const row of start.rows) assert.ok(row.controlLeft >= row.labelRight, JSON.stringify(row));
});

test("the section heading is the field's one label, and the field fills the section", async () => {
  const seen = await onProcess({}, (page) => page.evaluate(`(() => {
    const field = document.querySelector("${FIELD}");
    const group = field.closest(".set-group");
    return { labels: [...field.labels].map((label) => label.textContent), inHead: Boolean(field.labels[0]?.closest(".set-head h3")), slack: group ? group.getBoundingClientRect().width - field.getBoundingClientRect().width : -1 };
  })()`));
  assert.deepEqual(seen.labels, ["Instructions for agents"]);
  assert.equal(seen.inHead, true);
  assert.equal(seen.slack, 34, JSON.stringify(seen));
});

test("the instructions field uses the mono face at 13 on 20, as the approved page draws it", async () => {
  const seen = await onProcess({}, (page) => page.evaluate(`(() => {
    const style = getComputedStyle(document.querySelector("${FIELD}"));
    return { family: style.fontFamily, mono: getComputedStyle(document.documentElement).getPropertyValue("--mono"), size: style.fontSize, line: style.lineHeight };
  })()`));
  const plain = (font) => font.replace(/["'\s]/g, "");
  assert.equal(plain(seen.family), plain(seen.mono));
  assert.equal(seen.size, "13px");
  assert.equal(seen.line, "20px");
});

test("with nothing typed there is no mark, Save waits and Discard edits is absent", async () => {
  const seen = await onProcess({}, async (page) => ({ sections: await read(page), words: await say(page), off: await saveOff(page) }));
  assert.equal(seen.sections[1].mark, null);
  assert.equal(seen.off, true);
  assert.match(seen.words, /26 bytes of 64\.0 KB/);
  assert.doesNotMatch(seen.words, /Not saved yet|Discard edits/);
});

test("typing marks the section changed and wakes Save, and Discard edits puts it back", async () => {
  await onProcess({}, async (page) => {
    await typed(page, "More. ");
    const [, edited] = await read(page);
    assert.equal(edited.mark, "changed");
    assert.match(await say(page), /Not saved yet\./);
    assert.equal(await saveOff(page), false);
    await button(page, "Discard edits");
    const [, back] = await read(page);
    assert.equal(back.mark, null);
    assert.equal(await value(page), "Use tdd for every change.\n");
    assert.equal(await saveOff(page), true);
  });
});

test("Save sends the draft and nothing is saved before the click", async () => {
  await onProcess({}, async (page) => {
    await typed(page, "More. ");
    assert.ok(!(await sent(page)).includes("desktop_save_process_text"));
    await button(page, "Save");
    assert.ok((await sent(page)).includes("desktop_save_process_text"));
  });
});

test("As agents read it follows the draft, tags skills, and goes away with an empty draft", async () => {
  await onProcess({ saved: "empty" }, async (page) => {
    assert.deepEqual((await read(page)).map((section) => section.heading), ["Start from", "Instructions for agents", "Edit with AI"]);
    await typed(page, "Run tdd now.");
    const sections = await read(page);
    assert.equal(sections.at(-1).heading, "As agents read it");
    assert.equal(await page.evaluate('Fixture.wordsOf(".set-group .skill-tag")'), "tdd");
  });
});

test("the default process opens a preview first, Cancel changes nothing, Put in the editor saves nothing", async () => {
  await onProcess({}, async (page) => {
    await button(page, "Use the CrazyAgent default process");
    const shown = await dialog(page);
    assert.match(shown, /CrazyAgent default process/);
    assert.match(shown, /The shipped process\./);
    assert.match(shown, /nothing is saved until you click Save/);
    await inDialog(page, "Cancel");
    assert.equal(await open(page), 0);
    assert.equal(await value(page), "Use tdd for every change.\n");
    await button(page, "Use the CrazyAgent default process");
    await inDialog(page, "Put in the editor");
    assert.equal(await value(page), "The shipped process.\n");
    assert.equal((await read(page))[1].mark, "changed");
    assert.ok(!(await sent(page)).includes("desktop_save_process_text"));
  });
});

test("a picked file opens its preview and a closed chooser changes nothing", async () => {
  await onProcess({}, async (page) => {
    await button(page, "Import from file");
    const shown = await dialog(page);
    assert.match(shown, /Instructions from your file/);
    assert.match(shown, /Read once from the file you picked/);
    assert.match(shown, /From my file\./);
  });
  await onProcess({ file: "closed" }, async (page) => {
    await button(page, "Import from file");
    assert.equal(await open(page), 0);
    assert.equal(await page.evaluate("Fixture.alert()").then((alert) => alert.words), "");
    assert.equal(await value(page), "Use tdd for every change.\n");
  });
});

test("a seed that fails says so below the top line and keeps the editor", async () => {
  await onProcess({ shipped: "error" }, async (page) => {
    await button(page, "Use the CrazyAgent default process");
    const alert = await page.evaluate("Fixture.alert()");
    assert.match(alert.words, /engine_failed/);
    assert.equal(alert.belowTop, true);
    assert.equal(await open(page), 0);
    assert.equal(await value(page), "Use tdd for every change.\n");
  });
});

test("a save that fails says so and keeps the draft", async () => {
  await onProcess({ save: "error" }, async (page) => {
    await typed(page, "More. ");
    const draft = await value(page);
    await button(page, "Save");
    assert.match((await page.evaluate("Fixture.alert()")).words, /engine_failed/);
    assert.equal(await value(page), draft);
    assert.equal((await read(page))[1].mark, "changed");
  });
});

test("a stale save shows the conflict, holds Save, and Reload takes the saved text", async () => {
  await onProcess({ save: "stale" }, async (page) => {
    await typed(page, "More. ");
    const draft = await value(page);
    await button(page, "Save");
    const words = await say(page);
    assert.match(words, /Saved now:/);
    assert.match(words, /Someone else saved this\./);
    assert.match(words, /Keep mine/);
    assert.equal(await value(page), draft);
    assert.equal(await saveOff(page), true);
    await button(page, "Reload");
    assert.equal(await value(page), "Someone else saved this.\n");
  });
});

test("a skill link over unsaved text asks first, and Keep editing stays", async () => {
  await onProcess({}, async (page) => {
    await typed(page, "More. ");
    const draft = await value(page);
    await press(page, ".set-group .skill-tag", "tdd");
    const shown = await dialog(page);
    assert.match(shown, /Open tdd without saving\?/);
    assert.match(shown, /Your instructions changes will be lost\./);
    await inDialog(page, "Keep editing");
    assert.equal(await open(page), 0);
    assert.equal(await value(page), draft);
    assert.doesNotMatch(await page.evaluate("location.hash"), /skill=/);
  });
});

test("a skill link with nothing unsaved opens that skill in Plugins and skills", async () => {
  const hash = await onProcess({}, async (page) => {
    await press(page, ".set-group .skill-tag", "tdd");
    return page.evaluate("location.hash");
  });
  assert.equal(hash, "#/settings/plugins?skill=skill-0123456789abcdef01234567");
});

test("a text over 64 KB is refused at the field and Save stays off", async () => {
  await onProcess({ saved: "large" }, async (page) => {
    assert.equal(await page.evaluate(`Fixture.invalid("${FIELD}")`), "true");
    assert.match(await page.evaluate('Fixture.wordsOf(".field-error")'), /This text is over 64 KB\./);
    assert.equal(await saveOff(page), true);
  });
});

for (const [state, words] of [["error", /engine_failed/], ["missing", /Instructions isn't ready yet/]]) {
  test(`the ${state} read keeps the top line and says why, with no sections`, async () => {
    const seen = await onProcess({ saved: state }, async (page) => ({ heading: (await top(page))?.heading, words: await say(page), sections: await read(page) }));
    assert.equal(seen.heading, "Process");
    assert.match(seen.words, words);
    assert.deepEqual(seen.sections, []);
  });
}

test("the read still running keeps the top line and draws no sections", async () => {
  const seen = await onProcess({ saved: "pending" }, async (page) => ({ heading: (await top(page))?.heading, sections: await read(page), field: await page.evaluate(`Fixture.count("${FIELD}")`) }));
  assert.equal(seen.heading, "Process");
  assert.deepEqual(seen.sections, []);
  assert.equal(seen.field, 0);
});

test("real Tab keys walk the seeds, the field, Edit with AI and its switch, then the skill link, past the waiting Save", async () => {
  const walked = await onProcess({}, (page) => tabWalk(page, 6));
  assert.deepEqual(walked, ["Import from file", "Use the CrazyAgent default process", "Use tdd for every change.", "Edit with AI", "Edit with AI for the app instructions", "Skill tdd: Test first."]);
});

for (const theme of ["light", "dark"]) {
  test(`help, purpose lines, the size line and the mark read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onProcess({}, async (page) => {
      await typed(page, "More. ");
      return {
        help: await page.evaluate('Fixture.contrast(".set-help")'),
        about: await page.evaluate('Fixture.contrast(".set-head p")'),
        size: await page.evaluate('Fixture.contrast(".set-group .helper")'),
        mark: await page.evaluate('Fixture.contrast(".set-changed")'),
      };
    }, 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
