import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const kept = await import("../src/screens/team/kept-terminals.ts");

/** A kept view for the set's rows: its key, whether a panel shows it, and how often it was
 * let go. Inputs: the terminal name, the project and the pane. Output: the view. */
function view(terminal, project = "p1", pane = null) {
  return { key: { project, terminal, pane }, shown: false, drops: 0, drop() { this.drops += 1; } };
}

test("DEC-187: the owner keeps the last two terminals: a third one opened drops the least recently used", () => {
  const set = new kept.KeptSet(kept.KEPT_MAX);
  const master = view("team-1");
  const lead = view("team-2");
  const third = view("team-3");
  assert.deepEqual(set.add(master), []);
  assert.deepEqual(set.add(lead), []);
  set.touch(master.key);
  assert.deepEqual(set.add(third), [lead], "the lead was used before the master was shown again");
  assert.deepEqual(set.keys(), [third.key, master.key]);
  assert.equal(set.get(lead.key), undefined);
  assert.equal(set.size, 2);
});

test("DEC-187: a view a panel shows is never dropped, and the set comes back to two once it is hidden", () => {
  const set = new kept.KeptSet(kept.KEPT_MAX);
  const master = view("team-1");
  const side = view("team-2");
  master.shown = true;
  side.shown = true;
  set.add(master);
  set.add(side);
  const agents = view("team-3");
  agents.shown = true;
  assert.deepEqual(set.add(agents), [], "three shown views stay while all three are shown");
  assert.equal(set.size, 3);
  side.shown = false;
  assert.deepEqual(set.trim(), [side]);
  assert.deepEqual(set.keys(), [agents.key, master.key]);
});

test("DEC-187: a lead and its teammate known only by its pane are two views, and a key is kept once", () => {
  const set = new kept.KeptSet(kept.KEPT_MAX);
  const lead = view("team-2");
  const mate = view("team-2", "p1", "%4");
  set.add(lead);
  set.add(mate);
  assert.equal(set.get({ project: "p1", terminal: "team-2", pane: "%4" }), mate);
  assert.equal(set.get({ project: "p1", terminal: "team-2", pane: null }), lead);
  assert.throws(() => set.add(view("team-2", "p1", "%4")), /a key is kept once/);
  assert.notEqual(kept.keyId(lead.key), kept.keyId(mate.key));
  assert.notEqual(kept.keyId({ project: "p1", terminal: "team-2", pane: null }), kept.keyId({ project: "p2", terminal: "team-2", pane: null }));
});

test("DEC-187: nothing is kept after a project is closed or the session starts again, shown or not", () => {
  const set = new kept.KeptSet(kept.KEPT_MAX);
  const one = view("team-1", "p1");
  const two = view("team-2", "p2");
  two.shown = true;
  set.add(one);
  set.add(two);
  assert.deepEqual(set.dropWhere((key) => key.project === "p2"), [two]);
  assert.deepEqual(set.keys(), [one.key]);
  assert.deepEqual(set.dropWhere(() => true), [one]);
  assert.equal(set.size, 0);
});

test("DEC-187: a hidden view's output is held and written in one piece, in order, and a full hold is written at once", () => {
  const hold = new kept.HiddenOutput();
  assert.equal(hold.take(), null);
  for (let at = 0; at < 100; at += 1) assert.equal(hold.push(Uint8Array.of(at % 256)), false);
  assert.equal(hold.bytes, 100);
  const all = hold.take();
  assert.deepEqual([...all], Array.from({ length: 100 }, (_, at) => at % 256));
  assert.equal(hold.bytes, 0);
  assert.equal(hold.take(), null);
  const big = new Uint8Array(kept.HIDDEN_BYTES_MAX - 1);
  assert.equal(hold.push(big), false);
  assert.equal(hold.push(Uint8Array.of(7)), true, "the byte that fills the hold asks for a write now");
  assert.throws(() => hold.push(Uint8Array.of(8)), /a full hold is taken before the next chunk/);
  assert.equal(hold.take().length, kept.HIDDEN_BYTES_MAX);
});

test("DEC-187: a view loaded ahead of use goes behind the views the owner opened and never drops one", () => {
  const set = new kept.KeptSet(kept.KEPT_MAX);
  const opened = view("team-1");
  set.add(opened);
  const early = view("team-2");
  assert.equal(set.roomBehind(early.key), true);
  set.keepBehind(early);
  assert.deepEqual(set.keys(), [opened.key, early.key], "the preloaded view is the least recently used");
  const late = view("team-3");
  assert.equal(set.roomBehind(late.key), false, "a full set has no room behind");
  assert.equal(set.roomBehind(opened.key), false, "a kept key has no second place");
  assert.throws(() => set.keepBehind(late), /a view kept behind takes a free place/);
  assert.equal(opened.drops + early.drops, 0);
});

test("DEC-187: the remembered terminals read back only as exact entries, a key once, at most two, and sizes in bounds", () => {
  const entry = (patch = {}) => ({ project: "p1", terminal: "team-1", pane: null, width: 640, height: 400, ...patch });
  for (const text of [null, "", "not json", "{}", "7", JSON.stringify({ project: "p1" })]) assert.deepEqual(kept.savedViews(text), [], `${text} reads as none`);
  const good = kept.savedViews(JSON.stringify([entry(), entry({ terminal: "team-2", pane: "%4", width: null, height: null })]));
  assert.deepEqual(good, [
    { key: { project: "p1", terminal: "team-1", pane: null }, size: { width: 640, height: 400 } },
    { key: { project: "p1", terminal: "team-2", pane: "%4" }, size: null },
  ]);
  const bad = [
    entry({ project: null }), entry({ project: "" }), entry({ terminal: 7 }), entry({ pane: "" }), entry({ pane: undefined }),
    entry({ terminal: "x".repeat(kept.KEPT_TEXT_MAX + 1) }), entry({ width: 0 }), entry({ width: kept.KEPT_PX_MAX + 1 }),
    entry({ width: 10.5 }), entry({ height: null }), entry({ width: "640" }), null, "team-1",
  ];
  for (const item of bad) assert.deepEqual(kept.savedViews(JSON.stringify([item])), [], `${JSON.stringify(item)} is skipped`);
  const many = kept.savedViews(JSON.stringify([entry(), entry(), entry({ terminal: "team-2" }), entry({ terminal: "team-3" })]));
  assert.deepEqual(many.map((saved) => saved.key.terminal), ["team-1", "team-2"], "a repeated key counts once and the cap holds");
  assert.deepEqual(kept.savedViews(kept.savedText(good)), good, "the stored text reads back the same");
});

test("DEC-187: showing a terminal puts it first and keeps its known size, and a resize keeps the order", () => {
  const one = { key: { project: "p1", terminal: "team-1", pane: null }, size: { width: 600, height: 300 } };
  const two = { key: { project: "p2", terminal: "team-2", pane: null }, size: null };
  const three = { key: { project: "p1", terminal: "team-3", pane: "%2" }, size: null };
  let saved = kept.shownFirst([], one);
  saved = kept.shownFirst(saved, two);
  assert.deepEqual(saved, [two, one]);
  saved = kept.shownFirst(saved, { key: one.key, size: null });
  assert.deepEqual(saved, [one, two], "a shown view not yet sized keeps the size it had");
  saved = kept.shownFirst(saved, three);
  assert.deepEqual(saved, [three, one], "the least recent goes past the cap");
  saved = kept.resized(saved, one.key, { width: 800, height: 500 });
  assert.deepEqual(saved, [three, { key: one.key, size: { width: 800, height: 500 } }]);
  assert.deepEqual(kept.resized(saved, two.key, { width: 1, height: 1 }), saved, "a key not remembered changes nothing");
});

test("DEC-187: storage that refuses reads as no remembered terminal and a refused write changes nothing", () => {
  const before = globalThis.window;
  const store = new Map();
  try {
    globalThis.window = { localStorage: { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); } } };
    assert.deepEqual(kept.readSaved(), []);
    assert.doesNotThrow(() => kept.writeSaved([]));
    globalThis.window = { localStorage: { getItem: (name) => store.get(name) ?? null, setItem: (name, value) => store.set(name, value) } };
    const one = { key: { project: "p1", terminal: "team-1", pane: null }, size: null };
    kept.writeSaved([one]);
    assert.deepEqual(kept.readSaved(), [one]);
    assert.deepEqual([...store.keys()], [kept.KEPT_STORE_KEY]);
  } finally {
    globalThis.window = before;
  }
});
