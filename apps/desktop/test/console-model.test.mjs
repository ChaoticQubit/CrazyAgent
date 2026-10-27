import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const model = await import("../src/screens/project/console-model.ts");

const key = (patch) => ({ key: "Enter", shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, isComposing: false, keyCode: 13, ...patch });
const node = (id, kind, label) => ({ id, kind, parent: null, label, harness: "codex", status: "working", status_label: null, account: null, agents: null, terminal: null });

test("typing / at the start opens the command menu, filtered by what follows", () => {
  assert.deepEqual(model.filterCommands("/").map((command) => command.name), ["help", "clear", "agents", "changes", "usage", "process", "stop"]);
  assert.deepEqual(model.filterCommands("/c").map((command) => command.name), ["clear", "changes"]);
  assert.deepEqual(model.filterCommands("/ST").map((command) => command.name), ["stop"]);
  assert.deepEqual(model.filterCommands("/ess").map((command) => command.name), ["process"]);
  assert.deepEqual(model.filterCommands("/zzz"), []);
  assert.deepEqual(model.filterCommands("hello /stop"), [], "only a / at the start of the prompt opens the menu");
  assert.deepEqual(model.filterCommands("/stop now"), [], "the menu closes once the command has a space after it");
  assert.deepEqual(model.filterCommands(" /stop"), []);
});

test("a submitted line is a message, a known command, an unknown command or nothing", () => {
  assert.deepEqual(model.submitted("   "), { kind: "empty" });
  assert.deepEqual(model.submitted("  fix the login  "), { kind: "message", text: "fix the login" });
  assert.equal(model.submitted("/stop").kind, "command");
  assert.equal(model.submitted("/Agents ").command?.name, "agents");
  assert.deepEqual(model.submitted("/deploy prod"), { kind: "unknown", name: "deploy" });
  assert.equal(model.unknownLine("deploy"), "Unknown command /deploy. Type /help to see the commands.");
  assert.deepEqual(model.submitted("/Users/owner/plant-shop/src/main.rs is broken"), { kind: "message", text: "/Users/owner/plant-shop/src/main.rs is broken" },
    "a path at the start is a message, not a command");
  assert.deepEqual(model.submitted("/"), { kind: "message", text: "/" });
});

test("commands are data: each says what it does and where it goes", () => {
  for (const command of model.CONSOLE_COMMANDS) assert.ok(command.help.length > 0, command.name);
  assert.deepEqual(model.commandRoute("agents", "p-1"), { name: "project", id: "p-1", tab: "agents" });
  assert.deepEqual(model.commandRoute("changes", "p-1"), { name: "project", id: "p-1", tab: "changes" });
  assert.deepEqual(model.commandRoute("process", "p-1"), { name: "project", id: "p-1", tab: "process" });
  assert.deepEqual(model.commandRoute("usage", "p-1"), { name: "settings", tab: "usage" });
  assert.deepEqual(model.CONSOLE_COMMANDS.find((command) => command.name === "clear")?.action, { kind: "clear" });
  assert.deepEqual(model.CONSOLE_COMMANDS.find((command) => command.name === "stop")?.action, { kind: "stop" });
  assert.match(model.helpLines().join("\n"), /\/agents\s+Open the Agents tab/);
});

test("Enter sends, Shift+Enter and Ctrl+J add a line, Esc clears, and nothing acts while an IME composes", () => {
  const idle = { menu: false, browsing: false };
  assert.equal(model.consoleKey(key({}), idle), "send");
  assert.equal(model.consoleKey(key({ shiftKey: true }), idle), "newline");
  assert.equal(model.consoleKey(key({ key: "j", ctrlKey: true, keyCode: 74 }), idle), "newline");
  assert.equal(model.consoleKey(key({ key: "J", ctrlKey: true, shiftKey: true, keyCode: 74 }), idle), null, "Ctrl+Shift+J is left alone");
  assert.equal(model.consoleKey(key({ key: "Escape", keyCode: 27 }), idle), "clear");
  assert.equal(model.consoleKey(key({ isComposing: true }), idle), null, "the Enter that picks composed text never sends");
  assert.equal(model.consoleKey(key({ keyCode: 229 }), idle), null, "WebKit reports composing Enter as key code 229");
  assert.equal(model.consoleKey(key({ key: "j", ctrlKey: true, isComposing: true }), idle), null);
  assert.equal(model.consoleKey(key({ isComposing: true }), { menu: true, browsing: false }), null);
  assert.equal(model.consoleKey(key({ key: "a", keyCode: 65 }), idle), null);
});

test("with a menu open, the arrows move, Enter runs and Tab completes the choice, Esc closes it", () => {
  const menu = { menu: true, browsing: true };
  assert.equal(model.consoleKey(key({ key: "ArrowDown", keyCode: 40 }), menu), "menu-down");
  assert.equal(model.consoleKey(key({ key: "ArrowUp", keyCode: 38 }), menu), "menu-up");
  assert.equal(model.consoleKey(key({}), menu), "menu-pick");
  assert.equal(model.consoleKey(key({ key: "Tab", keyCode: 9 }), menu), "menu-complete");
  assert.equal(model.consoleKey(key({ key: "Escape", keyCode: 27 }), menu), "menu-close");
  assert.equal(model.consoleKey(key({ shiftKey: true }), menu), "newline");
  assert.equal(model.moveChoice(0, 3, 1), 1);
  assert.equal(model.moveChoice(2, 3, 1), 0);
  assert.equal(model.moveChoice(0, 3, -1), 2);
  assert.equal(model.moveChoice(5, 0, 1), 0);
});

test("Up and Down recall sent lines only while the prompt is empty or shows an unedited recalled line", () => {
  assert.equal(model.consoleKey(key({ key: "ArrowUp", keyCode: 38 }), { menu: false, browsing: true }), "history-back");
  assert.equal(model.consoleKey(key({ key: "ArrowDown", keyCode: 40 }), { menu: false, browsing: true }), "history-forward");
  assert.equal(model.consoleKey(key({ key: "ArrowUp", keyCode: 38 }), { menu: false, browsing: false }), null, "Up moves the caret in a typed prompt");
  const history = ["first", "second", "third"];
  assert.deepEqual(model.recall(history, null, -1), { index: 2, text: "third" });
  assert.deepEqual(model.recall(history, 2, -1), { index: 1, text: "second" });
  assert.deepEqual(model.recall(history, 0, -1), { index: 0, text: "first" }, "Up stops at the oldest line");
  assert.deepEqual(model.recall(history, 1, 1), { index: 2, text: "third" });
  assert.deepEqual(model.recall(history, 2, 1), { index: null, text: "" }, "Down past the newest line empties the prompt");
  assert.deepEqual(model.recall(history, null, 1), { index: null, text: "" });
  assert.deepEqual(model.recall([], null, -1), { index: null, text: "" });
  assert.deepEqual(model.remember(["a", "b"], "  b "), ["a", "b"], "the same line twice in a row is kept once");
  assert.deepEqual(model.remember(["a"], "   "), ["a"]);
  const long = Array.from({ length: model.HISTORY_MAX }, (_, index) => `line ${index}`);
  const kept = model.remember(long, "newest");
  assert.equal(kept.length, model.HISTORY_MAX);
  assert.equal(kept[0], "line 1");
  assert.equal(kept.at(-1), "newest");
});

test("@ completes the master and every sub-agent by its handle, and files when a list is given", () => {
  const agents = model.agentMentions([
    node("m", "master", "Master"),
    node("g", "harness_group", "codex"),
    node("team-1", "agent", "Ada (@onboarding-fix)"),
    node("team-2", "agent", "Grace"),
    node("team-3", "agent", "Sub-agent 7"),
    node("team-4", "agent", "Linus (@onboarding-fix)"),
  ]);
  assert.deepEqual(agents.map((mention) => mention.handle), ["master", "onboarding-fix", "grace", "sub-agent-7"]);
  assert.equal(agents[1].label, "Ada (@onboarding-fix)");
  assert.equal(agents[0].kind, "agent");
  assert.equal(model.agentMentions([]).map((mention) => mention.handle).join(), "master", "@master is always there");
  assert.equal(model.toHandle("  Ünïcode Name_two.three/four "), "ncode-name-two-three-four");
  assert.equal(model.toHandle("!!!"), null);
  assert.equal(model.toHandle("x".repeat(80))?.length, model.HANDLE_CHARS_MAX);
  const files = model.fileMentions(["src/main.rs", "docs/a file.md", "README.md", ...Array.from({ length: model.FILES_MAX + 10 }, (_, index) => `f${index}.rs`)]);
  assert.equal(files.length, model.FILES_MAX);
  assert.deepEqual(files.slice(0, 2).map((mention) => [mention.handle, mention.kind]), [["src/main.rs", "file"], ["README.md", "file"]],
    "a path with a space cannot be one tag, so it is left out");
  assert.deepEqual(model.fileMentions(["src/\u202etxt.exe", "a\u2067b.rs", "c\u200fd.rs", "ok.rs"]).map((mention) => mention.handle), ["ok.rs"],
    "a path holding a direction character would show reordered once inserted, so it is left out");
});

test("@ opens only at a word start, matches handles and labels, and inserts the tag with a space", () => {
  assert.deepEqual(model.mentionAt("ask @onb", 8), { start: 4, query: "onb" });
  assert.deepEqual(model.mentionAt("@", 1), { start: 0, query: "" });
  assert.equal(model.mentionAt("mail me@host", 12), null, "an @ inside a word is not a tag");
  assert.equal(model.mentionAt("ask @ada now", 12), null, "the caret is past the tag");
  assert.deepEqual(model.mentionAt("ask @ada now", 8), { start: 4, query: "ada" });
  const agents = model.agentMentions([node("team-1", "agent", "Ada (@onboarding-fix)"), node("team-2", "agent", "Grace")]);
  assert.deepEqual(model.matchMentions(agents, "").map((mention) => mention.handle), ["master", "onboarding-fix", "grace"]);
  assert.deepEqual(model.matchMentions(agents, "ON").map((mention) => mention.handle), ["onboarding-fix"]);
  assert.deepEqual(model.matchMentions(agents, "ada").map((mention) => mention.handle), ["onboarding-fix"], "the label matches too");
  assert.deepEqual(model.matchMentions(agents, "zz"), []);
  assert.deepEqual(model.insertMention("ask @onb please", 8, { start: 4, query: "onb" }, agents[1]), { text: "ask @onboarding-fix please", caret: 20 });
  assert.deepEqual(model.insertMention("@", 1, { start: 0, query: "" }, agents[0]), { text: "@master ", caret: 8 });
});

test("tags in a sent line are marked only when they name a known agent or file", () => {
  const known = new Set(["master", "onboarding-fix", "src/main.rs"]);
  assert.deepEqual(model.tagSegments("ask @master and @onboarding-fix about @src/main.rs, not me@host or @nobody", known), [
    { text: "ask ", tag: false }, { text: "@master", tag: true }, { text: " and ", tag: false }, { text: "@onboarding-fix", tag: true },
    { text: " about ", tag: false }, { text: "@src/main.rs", tag: true }, { text: ", not me@host or @nobody", tag: false },
  ]);
  assert.deepEqual(model.tagSegments("", known), []);
});

test("the prompt grows with its lines up to eight", () => {
  assert.equal(model.promptRows(""), 1);
  assert.equal(model.promptRows("a\nb\nc"), 3);
  assert.equal(model.promptRows("\n".repeat(20)), model.PROMPT_ROWS_MAX);
  assert.equal(model.PROMPT_ROWS_MAX, 8);
});
