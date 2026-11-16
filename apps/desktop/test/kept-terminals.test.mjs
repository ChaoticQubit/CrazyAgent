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
