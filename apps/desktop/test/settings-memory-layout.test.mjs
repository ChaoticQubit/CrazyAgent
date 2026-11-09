import assert from "node:assert/strict";
import test from "node:test";
import { CONTRAST_MIN, argsOf, commands, contrasts, count, dialog, doublePress, onTab, press, read, say, stroke, tabStops, toasts, top, typeInto } from "./settings-agents-page.mjs";

const PURPOSE = "Facts agents keep across projects, with who saved each one. Agents read them at launch.";
const WRITES = "When an agent saves a memory";
const SAVED = "Saved memories";
const NOTE = "Something agents should remember (app-wide)";
const ESCAPE = { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 };
const on = (scene, check, width, theme) => onTab("memory", scene, check, width, theme);

function section(sections, heading) {
  const found = sections.find((item) => item.heading === heading);
  assert.ok(found, `${heading} in ${JSON.stringify(sections.map((item) => item.heading))}`);
  return found;
}

test("the tab shows its title and purpose, then its five sections", async () => {
  const seen = await on({}, async (page) => ({ top: await top(page), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Memory", JSON.stringify(seen.top));
  assert.equal(seen.top.purpose, PURPOSE);
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((item) => item.heading), ["Agent memory writes", "Suggested", SAVED, "Add memory", "Import memory"]);
});

test("the owner's memories carry no changed mark", async () => {
  assert.equal(await on({}, (page) => count(page, ".set-changed")), 0);
});

test("the writes setting has its label left, its choice right and its warning under it", async () => {
  const sections = await on({}, read);
  const [line] = section(sections, "Agent memory writes").rows;
  assert.equal(line.label, WRITES);
  assert.ok(line.controlLeft > line.labelRight, JSON.stringify(line));
  assert.match(line.help, /Agents' memories are saved at once\. A sub-agent that reads hostile content could save a memory that steers every agent; delete any you don't trust\./);
});

test("a press on the writes label focuses the choice and a real key changes it", async () => {
  const seen = await on({}, async (page) => {
    await press(page, 'label[for="memory-agent-writes"]');
    const focused = await page.evaluate("Fixture.focused()");
    await stroke(page, { key: "a", code: "KeyA", text: "a", windowsVirtualKeyCode: 65 });
    return { focused, sent: await argsOf(page, "desktop_set_agent_memory_writes"), sections: await read(page), toasts: await toasts(page) };
  });
  assert.equal(seen.focused.id, "memory-agent-writes");
  assert.deepEqual(seen.sent.map((args) => args.writes), ["ask"]);
  assert.match(section(seen.sections, "Agent memory writes").rows[0].help, /Agents' memories wait here until you approve them\./);
  assert.deepEqual(seen.toasts, ["Agents now ask you before a memory is saved."]);
});

test("a saved memory has its text left, its source under it and its actions right", async () => {
  const seen = await on({}, async (page) => ({ sections: await read(page), notes: await count(page, ".pj-note-text") }));
  const [first, waiting] = section(seen.sections, SAVED).rows;
  assert.equal(first.label, "The owner prefers Rust for engines.");
  assert.equal(first.help, "App-wide");
  assert.deepEqual(first.buttons, ["Delete"]);
  assert.ok(first.controlLeft > first.labelRight, JSON.stringify(first));
  assert.equal(waiting.help, "Waiting for your approval. Harbor Notes only, asked by agent-7");
  assert.deepEqual(waiting.buttons, ["Approve", "Delete"]);
  assert.equal(seen.notes, 3);
});

test("a very long memory wraps inside its text and leaves the actions alone", async () => {
  const [row] = section(await on({ memories: "long" }, read), SAVED).rows;
  assert.equal(row.label.length, 400);
  assert.equal(row.labelSpills, false, JSON.stringify(row));
  assert.ok(row.controlLeft >= row.labelRight, JSON.stringify(row));
  assert.deepEqual(row.buttons, ["Delete"]);
});

test("Approve reaches the engine and says so", async () => {
  const seen = await on({}, async (page) => {
    await press(page, 'button', "Approve");
    return { sent: await argsOf(page, "desktop_approve_memory"), toasts: await toasts(page) };
  });
  assert.deepEqual(seen.sent.map((args) => args.id), ["m2"]);
  assert.deepEqual(seen.toasts, ["Memory approved. Agents get it from their next launch."]);
});

for (const [wanted, command] of [["Approve", "desktop_approve_memory"], ["Save to memory", "desktop_review_memory"], ["Dismiss", "desktop_review_memory"]]) {
  test(`a double press on ${wanted} sends it once`, async () => {
    const sent = await on({ hold: command }, async (page) => {
      await doublePress(page, "button", wanted);
      return argsOf(page, command);
    });
    assert.equal(sent.length, 1, JSON.stringify(sent));
  });
}

test("a suggestion has its text and project left and Save to memory, Edit and Dismiss right", async () => {
  const [row] = section(await on({}, read), "Suggested").rows;
  assert.equal(row.label, "Competitors charge per seat.");
  assert.equal(row.help, "From Harbor Notes");
  assert.deepEqual(row.buttons, ["Save to memory", "Edit", "Dismiss"]);
  assert.ok(row.controlLeft > row.labelRight, JSON.stringify(row));
});

test("Save to memory keeps the suggestion and makes it app-wide", async () => {
  const seen = await on({}, async (page) => {
    await press(page, "button", "Save to memory");
    return { review: await argsOf(page, "desktop_review_memory"), promote: await argsOf(page, "desktop_promote_memory"), toasts: await toasts(page) };
  });
  assert.deepEqual(seen.review.map((args) => args.decision), [{ decision: "Keep" }]);
  assert.deepEqual(seen.promote.map((args) => args.version), [4]);
  assert.deepEqual(seen.toasts, ["Saved to memory."]);
});

test("Dismiss rejects the suggestion and says so", async () => {
  const seen = await on({}, async (page) => {
    await press(page, "button", "Dismiss");
    return { review: await argsOf(page, "desktop_review_memory"), toasts: await toasts(page) };
  });
  assert.deepEqual(seen.review.map((args) => args.decision), [{ decision: "Reject" }]);
  assert.deepEqual(seen.toasts, ["Suggestion dismissed."]);
});

test("Edit opens the editor in place; Cancel and Escape go back, Save edited sends the new words", async () => {
  const seen = await on({}, async (page) => {
    await press(page, "button", "Edit");
    const editing = await say(page);
    await press(page, "button", "Cancel");
    const cancelled = await read(page);
    await press(page, "button", "Edit");
    await page.evaluate("document.querySelector('textarea[id^=memory-edit]').focus()");
    await stroke(page, ESCAPE);
    const escaped = await read(page);
    await press(page, "button", "Edit");
    await typeInto(page, "textarea[id^=memory-edit]", "Seats cost more.");
    await press(page, "button", "Save edited");
    return { editing, cancelled, escaped, review: await argsOf(page, "desktop_review_memory"), promote: await argsOf(page, "desktop_promote_memory"), toasts: await toasts(page) };
  });
  for (const part of ["Edit before saving (from Harbor Notes)", "Save edited", "Cancel"]) assert.ok(seen.editing.includes(part), `${part} in ${seen.editing}`);
  assert.deepEqual(section(seen.cancelled, "Suggested").rows[0].buttons, ["Save to memory", "Edit", "Dismiss"]);
  assert.deepEqual(section(seen.escaped, "Suggested").rows[0].buttons, ["Save to memory", "Edit", "Dismiss"]);
  assert.deepEqual(seen.review.map((args) => args.decision), [{ decision: "Edit", text: "Seats cost more." }]);
  assert.equal(seen.promote.length, 1);
  assert.deepEqual(seen.toasts, ["Saved to memory."]);
});

test("Saved memories holds its filters in its head, and Show needs a project", async () => {
  assert.deepEqual(section(await on({}, read), SAVED).actions, ["Show", "Search memory"]);
  assert.deepEqual(section(await on({ projects: "none" }, read), SAVED).actions, ["Search memory"]);
});

test("a search with no match says so, and the section stays", async () => {
  const seen = await on({}, async (page) => {
    await typeInto(page, "input[type=search]", "nothing like this");
    await page.evaluate("Fixture.settle(120)");
    return { says: await say(page), sections: await read(page) };
  });
  assert.ok(seen.says.includes("No memories match."), seen.says);
  assert.equal(section(seen.sections, SAVED).rows.length, 0);
});

test("Show older adds the next page and goes away", async () => {
  const seen = await on({ memories: "older" }, async (page) => {
    await press(page, "button", "Show older");
    return { reads: await argsOf(page, "desktop_memories"), sections: await read(page), says: await say(page) };
  });
  assert.equal(seen.reads.at(-1).before, 2);
  assert.equal(section(seen.sections, SAVED).rows.length, 3);
  assert.ok(seen.says.includes("An older memory."), seen.says);
  assert.ok(!seen.says.includes("Show older"), seen.says);
});

for (const [state, expected, writes] of [
  ["empty", ["No memories yet", "When agents find something worth keeping, it shows up here for you to save."], true],
  ["error", ["Try again"], false],
  ["missing", ["Memory isn't ready yet"], false],
]) {
  test(`the ${state} state stands inside Saved memories, under the title`, async () => {
    const seen = await on({ memories: state }, async (page) => ({ top: await top(page), says: await say(page), sections: await read(page) }));
    assert.equal(seen.top?.heading, "Memory", JSON.stringify(seen.top));
    section(seen.sections, SAVED);
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
    assert.equal(seen.sections.some((item) => item.heading === "Agent memory writes"), writes);
  });
}

test("while memories load the section holds its skeletons", async () => {
  const seen = await on({ memories: "pending" }, async (page) => ({ top: await top(page), skeletons: await count(page, ".skeleton"), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Memory", JSON.stringify(seen.top));
  assert.equal(seen.skeletons, 4);
  section(seen.sections, SAVED);
});

test("Try again reads the memories a second time", async () => {
  const reads = await on({ memories: "error" }, async (page) => {
    await press(page, "button", "Try again");
    return (await commands(page)).filter((command) => command === "desktop_memories");
  });
  assert.equal(reads.length, 2);
});

test("Delete asks first and sends only on the confirm, and Cancel sends nothing", async () => {
  const seen = await on({}, async (page) => {
    await press(page, ".set-row button", "Delete");
    const asking = await dialog(page);
    await press(page, "dialog[open] button", "Cancel");
    const open = await count(page, "dialog[open]");
    const before = await argsOf(page, "desktop_delete_memory");
    await press(page, ".set-row button", "Delete");
    await press(page, "dialog[open] button", "Delete");
    return { asking, open, before, sent: await argsOf(page, "desktop_delete_memory"), toasts: await toasts(page) };
  });
  for (const part of ["Delete this memory?", "Agents stop using it right away.", "Cancel", "Delete"]) assert.ok(seen.asking.includes(part), `${part} in ${seen.asking}`);
  assert.equal(seen.open, 0);
  assert.deepEqual(seen.before, []);
  assert.deepEqual(seen.sent.map((args) => args.id), ["m1"]);
  assert.deepEqual(seen.toasts, ["Memory deleted."]);
});

test("a change this build lacks shows the failure under the title", async () => {
  const alert = await on({ absent: "desktop_delete_memory" }, async (page) => {
    await press(page, ".set-row button", "Delete");
    await press(page, "dialog[open] button", "Delete");
    return page.evaluate("Fixture.alert()");
  });
  assert.match(alert.words, /This change isn't ready in this build yet\./);
  assert.equal(alert.belowTop, true, JSON.stringify(alert));
});

test("Add memory refuses an empty note and saves a written one", async () => {
  const seen = await on({}, async (page) => {
    const says = await say(page);
    await press(page, 'button[type="submit"]', "Add memory");
    const empty = await say(page);
    await typeInto(page, "#memory-note", "Keep the logs.");
    await press(page, 'button[type="submit"]', "Add memory");
    return { says, empty, sent: await argsOf(page, "desktop_add_memory"), toasts: await toasts(page) };
  });
  assert.ok(seen.says.includes(NOTE), seen.says);
  assert.ok(seen.empty.includes("Write something to remember."), seen.empty);
  assert.deepEqual(seen.sent.map(({ project, text }) => [project, text]), [[null, "Keep the logs."]]);
  assert.deepEqual(seen.toasts, ["Saved to memory."]);
});

test("Import memory picks a folder, imports into the project and reports the counts", async () => {
  const seen = await on({}, async (page) => {
    const says = await say(page);
    await press(page, "button", "Import memory");
    return { says, picked: await argsOf(page, "desktop_pick_folder"), imports: await argsOf(page, "desktop_import_memory"), after: await say(page), toasts: await toasts(page), select: await count(page, "#memory-import-project") };
  });
  assert.ok(seen.says.includes("Choose a folder of markdown memory files, such as a Claude Code project memory folder."), seen.says);
  assert.ok(seen.says.includes("Into project"), seen.says);
  assert.deepEqual(seen.picked.map((args) => args.purpose), ["memory"]);
  assert.equal(seen.imports.length, 1);
  assert.equal(seen.select, 1);
  assert.ok(seen.after.includes("Imported 2, skipped 1."), seen.after);
  assert.deepEqual(seen.toasts, ["Imported 2, skipped 1."]);
});

test("with no project Import memory says to add one first", async () => {
  const seen = await on({ projects: "none" }, async (page) => ({ says: await say(page), select: await count(page, "#memory-import-project") }));
  assert.ok(seen.says.includes("Add a project first. Imported memories belong to one project."), seen.says);
  assert.equal(seen.select, 0);
});

test("real Tab keys walk the setting, the suggestion, the filters, the memories and the forms", async () => {
  const walked = await on({}, (page) => tabStops(page, 13));
  assert.deepEqual(walked, [WRITES, "Save to memory", "Edit", "Dismiss", "Show", "Search memory", "Delete", "Approve", "Delete", NOTE, "Add memory", "Into project", "Import memory"]);
});

for (const theme of ["light", "dark"]) {
  test(`the purpose and help read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await on({}, (page) => contrasts(page, [".settings-top p", ".set-help", ".set-row .helper"]), 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
