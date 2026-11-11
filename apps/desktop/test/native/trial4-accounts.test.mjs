import assert from "node:assert/strict";
import { test } from "node:test";
import {
  accountStepVerdict, confirmVerdict, engineVerdict, graphifyVerdict, noLoop, providerVerdict, repeated, scratchName, signInRowsVerdict, tidy, listTotal,
} from "./trial4-accounts.mjs";

test("scratch names carry the trial 4 mark, the kind and the time, within the 60 character account name limit", () => {
  const name = scratchName("rename", 1793720000000);
  assert.match(name, /^ca-trial-4 rename [a-z0-9]+$/u);
  assert(name.length <= 60);
  assert.notEqual(scratchName("rename", 1793720000000), scratchName("rename", 1793720000001));
  assert.throws(() => scratchName("Bad Kind!", 1), /kind/u);
});

test("repeated names are found once each, ignoring case and spaces at the ends", () => {
  assert.deepEqual(repeated(["OpenAI", "Anthropic", "openai ", "Local", "Anthropic", "OpenAI"]), ["OpenAI", "Anthropic"]);
  assert.deepEqual(repeated(["OpenAI", "Anthropic"]), []);
});

test("Add account passes with one OpenAI and one Anthropic tile and fails on OP-45 and OP-58", () => {
  const tiles = ["OpenAI", "Anthropic", "OpenRouter", "OpenAI-compatible server", "Local model"];
  const methods = { OpenAI: ["ChatGPT subscription", "Use the Codex sign-in on this Mac", "OpenAI API key"], Anthropic: ["Claude subscription", "Anthropic API key"] };
  assert.match(providerVerdict({ tiles, methods }), /^5 providers, each once: OpenAI \(3 ways\), Anthropic \(2 ways\)/u);
  assert.throws(() => providerVerdict({ tiles: [...tiles, "OpenAI"], methods }), { message: "OP-45: Add account lists OpenAI more than once" });
  assert.throws(() => providerVerdict({ tiles: [...tiles, "Anthropic API"], methods }), { message: "OP-58: Add account lists Anthropic API beside Anthropic" });
  assert.throws(() => providerVerdict({ tiles, methods: { ...methods, Anthropic: ["Claude subscription", "Claude subscription"] } }),
    { message: "OP-45: Anthropic lists Claude subscription more than once" });
  assert.throws(() => providerVerdict({ tiles: ["OpenRouter"], methods: {} }), { message: "Add account shows no OpenAI tile" });
});

test("Home shows one sign-in row for the signed-out account and fails on OP-46 duplicates", () => {
  const name = "ca-trial-4 home k1";
  const rows = [`${name} isn't signed in.`, "Tidepool 2 new team notifications."];
  assert.equal(signInRowsVerdict({ name, rows }), `one Needs you row for ${name}: "${name} isn't signed in."`);
  assert.throws(() => signInRowsVerdict({ name, rows: [...rows, `${name} isn't signed in.`] }), { message: `OP-46: Home shows 2 rows for ${name}` });
  assert.throws(() => signInRowsVerdict({ name, rows: ["Tidepool 2 new team notifications."] }), { message: `Home shows no sign-in row for ${name}` });
});

test("Add an account is done only with an account ready for agents (OP-47)", () => {
  assert.match(accountStepVerdict({ panel: true, done: false, body: "No account is ready for agents yet. Sign one in or check its key or URL.", ready: 0, total: 2 }),
    /^Add an account reads not done with 0 of 2 accounts ready/u);
  assert.match(accountStepVerdict({ panel: true, done: true, body: "Work is ready for agents.", ready: 1, total: 3 }), /^Add an account reads Done with 1 of 3 accounts ready/u);
  assert.match(accountStepVerdict({ panel: false, done: null, body: "", ready: 4, total: 4 }), /^Get set up is hidden, every step done, with 4 of 4 accounts ready/u);
  assert.throws(() => accountStepVerdict({ panel: true, done: true, body: "", ready: 0, total: 1 }), { message: "OP-47: Add an account reads Done with 0 of 1 accounts ready for agents" });
  assert.throws(() => accountStepVerdict({ panel: false, done: null, body: "", ready: 0, total: 0 }), { message: "OP-47: Get set up is hidden as done with 0 of 0 accounts ready for agents" });
  assert.throws(() => accountStepVerdict({ panel: true, done: false, body: "", ready: 2, total: 2 }), { message: "Add an account reads not done with 2 of 2 accounts ready for agents" });
});

test("Graphify shows once as a tool and once as a pack entry, named apart, and the tool reads installed when present (OP-49)", () => {
  const rows = [
    { section: "Tools", title: "tmux", words: ["Found"], on: null },
    { section: "Tools", title: "Graphify command", words: ["Found"], on: null },
    { section: "Code graph", title: "Graphify skill for agents", words: ["On"], on: true },
  ];
  assert.deepEqual(graphifyVerdict(rows), { tool: rows[1], entry: rows[2] });
  assert.throws(() => graphifyVerdict([...rows, { section: "Tools", title: "graphify", words: ["Not installed"], on: null }]), { message: "OP-49: Graphify shows 2 times among the tools" });
  assert.throws(() => graphifyVerdict([rows[0], { ...rows[1], words: ["Not installed"] }, rows[2]]), { message: "not run: the Graphify command is not installed on this Mac (Not installed)" });
  assert.throws(() => graphifyVerdict([rows[0], rows[1], { ...rows[2], words: ["Not installed"], on: null }]),
    { message: "OP-49: the Graphify pack entry reads Not installed with no switch while the Graphify command reads Found" });
  assert.throws(() => graphifyVerdict([rows[0], rows[1], { ...rows[2], title: "Graphify command" }]), { message: "OP-49: the Graphify tool and pack entry share the name Graphify command" });
});

test("the app opened like the Dock passes only with projects listed and no engine error (OP-43)", () => {
  assert.equal(engineVerdict({ text: "Projects\nTidepool\nca-trial-4", projects: 2 }), "Home lists 2 projects with no engine error");
  assert.throws(() => engineVerdict({ text: "The engine is not running. Cannot connect to engine.", projects: 0 }), { message: "OP-43: the app says \"engine is not running\"" });
  assert.throws(() => engineVerdict({ text: "Projects", projects: 0 }), { message: "Home lists no project" });
});

test("a confirm passes when its title and body name what changes", () => {
  assert.equal(confirmVerdict({ title: "Delete ca-trial-4 pool k1?", body: "Its accounts stay in your list. Only the pool goes away.", wants: ["ca-trial-4 pool k1", "Only the pool goes away"] }),
    "\"Delete ca-trial-4 pool k1?\": Its accounts stay in your list. Only the pool goes away.");
  assert.throws(() => confirmVerdict({ title: "Delete pool?", body: "", wants: ["ca-trial-4 pool k1"] }), { message: "the confirm \"Delete pool?\" does not name ca-trial-4 pool k1" });
});

test("no screen loops back to assigned to a project (OP-56)", () => {
  assert.equal(noLoop("ca-trial-4 pool k1 deleted."), "ca-trial-4 pool k1 deleted.");
  assert.throws(() => noLoop("This pool is assigned to a project. Unassign it first."), { message: "OP-56: the app still says \"assigned to a project\"" });
});

test("tidy runs the cleanup after the check and never hides the check's own failure", async () => {
  const done = [];
  assert.equal(await tidy(async () => "seen", async () => { done.push("clean"); }), "seen");
  assert.deepEqual(done, ["clean"]);
  await assert.rejects(tidy(async () => { throw new Error("OP-44: the row is still there"); }, async () => { done.push("again"); }),
    { message: "OP-44: the row is still there" });
  assert.deepEqual(done, ["clean", "again"]);
  await assert.rejects(tidy(async () => { throw new Error("OP-44: the row is still there"); }, async () => { throw new Error("remove failed"); }),
    { message: "OP-44: the row is still there; cleanup also failed: remove failed" });
  await assert.rejects(tidy(async () => "seen", async () => { throw new Error("remove failed"); }),
    { message: "the check passed but its cleanup failed: remove failed" });
});

test("the Accounts count reads the total whether or not a search filters the list", () => {
  assert.equal(listTotal("3 accounts"), 3);
  assert.equal(listTotal("1 account"), 1);
  assert.equal(listTotal("1 of 1,204 accounts"), 1204);
  assert.equal(listTotal("0 of 7 accounts"), 7);
  assert.throws(() => listTotal("2 selected"), { message: "the Accounts count reads \"2 selected\", not a number of accounts" });
});
