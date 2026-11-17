import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const settings = await import("../src/screens/project/settings.tsx");
const api = await import("../src/api.ts");
const projects = await import("../src/api-projects.ts");

test("the sandbox section opens from its route and adds only absolute, new folders up to the limit", () => {
  assert.equal(settings.settingsSection("sandbox"), "sandbox");
  assert.deepEqual(settings.addReadPath([], "  /Users/me/shared  "), { paths: ["/Users/me/shared"] });
  assert.deepEqual(settings.addReadPath([], "~/shared"), { error: "Type the full path of a folder, starting with /." });
  assert.deepEqual(settings.addReadPath([], "relative/folder"), { error: "Type the full path of a folder, starting with /." });
  assert.deepEqual(settings.addReadPath(["/a"], "/a"), { error: "That folder is already in the list." });
  assert.deepEqual(settings.addReadPath([], "/bad\npath"), { error: "Each folder must be one plain path of up to 1024 characters." });
  const full = Array.from({ length: settings.READ_PATHS_MAX }, (_, index) => `/f${index}`);
  assert.deepEqual(settings.addReadPath(full, "/one-more"), { error: "A project can share at most 8 extra folders." });
});

test("every engine refusal for a read-only folder has its own sentence", () => {
  for (const code of ["read_path_not_absolute", "read_path_missing", "read_path_not_folder", "read_path_holds_home", "read_path_credentials",
    "read_path_engine_data", "read_path_refused", "read_path_owner_unknown", "read_paths_too_many", "read_paths_invalid"]) {
    assert.ok(api.KNOWN_ERROR_CODES.includes(code), code);
  }
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/extra_paths.rs", import.meta.url), "utf8");
  for (const code of engine.matchAll(/"(read_paths?_[a-z_]+)"/g)) assert.ok(api.KNOWN_ERROR_CODES.includes(code[1]), code[1]);
});

test("a save refused because the folders changed reads them again instead of resending the old version", () => {
  const receipt = (outcome, rejection = null) => ({ kind: "ok", value: { pending: false, outcome, rejection } });
  assert.equal(settings.savedStale(receipt("rejected", "stale_revision")), true, "another writer changed the folders since this screen read them");
  assert.equal(settings.savedStale({ kind: "error", code: "stale_revision", message: "" }), true);
  assert.equal(settings.savedStale(receipt("applied")), false);
  assert.equal(settings.savedStale(receipt("rejected", "read_path_credentials")), false, "an unsafe folder keeps the owner's list to fix");
  assert.equal(settings.savedStale({ kind: "not_ready" }), false);
  assert.equal(settings.savedStale({ kind: "ok", value: { pending: true, outcome: "uncertain", rejection: null } }), false, "an unconfirmed save is not a stale one");
  assert.match(settings.READ_PATHS_STALE, /changed since you opened them\. The list now shows the saved folders/);
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const save = source.slice(source.indexOf("  async function save() {\n    if (saved === null) return;"));
  assert.match(save, /if \(savedStale\(result\)\) \{\n      const shown = await reread\(\);/, "the stale save reads the folders and their version again");
  assert.ok(save.indexOf("savedStale(result)") < save.indexOf("failureCopy(result)"), "before the plain refusal copy");
  const reread = source.slice(source.indexOf("  async function reread()"), source.indexOf("  async function save() {\n    if (saved === null)"));
  assert.match(reread, /setSaved\(fresh\.value\);\n    setPaths\(fresh\.value\.paths\);/);
});

test("a hidden folders save sends the version it read, and a stale one reads the folders again instead of un-hiding one", () => {
  assert.match(settings.HIDDEN_PATHS_STALE, /changed since you opened them\. The list now shows the saved folders/);
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const start = source.indexOf("function HiddenSection(");
  const hidden = source.slice(start, source.indexOf("\n}\n", start));
  assert.match(hidden, /useState<HiddenPaths \| null>\(null\)/);
  assert.match(hidden, /saveHiddenPaths\(data\.id, paths, saved\.version, nextGeneration\(\)\)/, "the save sends the version the screen read");
  assert.match(hidden, /if \(savedStale\(result\)\) \{\n      const shown = await reread\(\);/, "a stale save reads the folders and their version again");
  const save = hidden.slice(hidden.indexOf("  async function save() {"));
  assert.ok(save.indexOf("savedStale(result)") < save.indexOf("failureCopy(result)"), "before the plain refusal copy");
  assert.match(hidden, /setSaved\(fresh\.value\);\n    setLatest\(fresh\.value\);\n    setPaths\(fresh\.value\.paths\);/);
  assert.ok(!hidden.includes("setSaved(paths)"), "an applied save reads the folders and their new version again");
  const api = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(api, /export function saveHiddenPaths\(id: string, paths: string\[\], version: number, generation: number\)/);
  assert.match(api, /call<IntentReceipt>\("desktop_set_project_hidden_paths", \{ id, version, paths, generation \}\)/);
});

test("the hidden folders section is not offered once the engine says this system has no hidden folders (DEC-194)", () => {
  assert.equal(settings.hiddenOffered(null), true, "loading still shows the section");
  assert.equal(settings.hiddenOffered({ paths: [], version: 0 }), true, "an engine without the flag offers it");
  assert.equal(settings.hiddenOffered({ paths: ["C:\\p\\secret"], version: 3, unavailable: true }), false, "an old saved list is not offered either");
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const start = source.indexOf("function HiddenSection(");
  const hidden = source.slice(start, source.indexOf("\n}\n", start));
  const gate = hidden.indexOf("if (!hiddenOffered(latest)) return null;");
  assert.ok(gate > 0, "the section draws nothing where the engine read says unavailable");
  assert.ok(gate > hidden.lastIndexOf("useEffect("), "after every hook");
  const api = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(api, /export type HiddenPaths = \{ paths: string\[\]; version: number; runs\?: HiddenRunKind\[\]; unavailable\?: boolean \};/);
});

test("the hidden folders section names the running agents that can still open a hidden folder and Restart them restarts them", () => {
  const agent = (id, label, sees) => ({ id, label, ...(sees === undefined ? {} : { sees_hidden: sees }) });
  const status = { agents: [agent(1, "Master", true), agent(2, "Ada", false), agent(3, "Cy", true), agent(4, "Bo")] };
  assert.deepEqual(settings.hiddenSeers(null), [], "nothing before the team is read");
  assert.deepEqual(settings.hiddenSeers(status).map((seer) => seer.id), [1, 3], "an engine without the flag lists none");
  assert.equal(settings.seersLine([agent(3, "Cy", true)]), "Cy started before a folder was hidden and can still open it.");
  assert.equal(settings.seersLine(settings.hiddenSeers(status)), "Master, Cy started before a folder was hidden and can still open it.");
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const start = source.indexOf("function HiddenSection(");
  const hidden = source.slice(start, source.indexOf("\n}\n", start));
  assert.match(hidden, /hiddenSeers\(data\.team\.result\?\.kind === "ok" \? data\.team\.result\.value : null\)/, "read from the team the page already polls");
  assert.match(hidden, /restartHiddenSeers\(data\.id, nextGeneration\(\)\)/);
  assert.match(hidden, />Restart them</);
  assert.match(hidden, /!fullAccess && seers\.length > 0/, "Full access hides nothing, so it lists no one");
  assert.match(hidden, /\{!fullAccess && <p className="pj-secondary">\{HIDDEN_SEERS_COVER\}<\/p>\}/, "an empty list never reads as nothing can open it");
  assert.match(settings.HIDDEN_SEERS_COVER, /^Team agents that started before you hid a folder, including ones CrazyAgent took over/);
  assert.match(settings.HIDDEN_SEERS_COVER, /Master turns, research jobs and overview, roadmap, onboarding, code graph and lockfile rebuild runs that started before are named here too, and keep their access until they end\.$/);
  const api = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(api, /call<IntentReceipt>\("desktop_team_restart_hidden_seers", \{ id, generation \}\)/);
  assert.equal(projects.failureCopy("team_no_hidden_seers"), "No running agent can still open a hidden folder.");
});

test("the hidden folders section names the engine runs that can still open a hidden folder, by kind, with no restart", () => {
  const saved = (runs) => ({ paths: ["/Users/me/project/secret"], version: 1, ...(runs === undefined ? {} : { runs }) });
  assert.equal(settings.hiddenRunsLine(null), null, "nothing before the folders are read");
  assert.equal(settings.hiddenRunsLine(saved()), null, "an engine without the list names none");
  assert.equal(settings.hiddenRunsLine(saved([])), null);
  assert.equal(settings.hiddenRunsLine(saved(["research"])), "Runs that can still open a hidden folder until they end: research job.");
  assert.equal(
    settings.hiddenRunsLine(saved(["master", "research", "overview", "roadmap", "onboarding", "code_graph", "lockfiles"])),
    "Runs that can still open a hidden folder until they end: master turn, research job, overview run, roadmap run, onboarding run, code graph build, lockfile rebuild run.",
  );
  assert.equal(settings.hiddenRunsLine(saved(["research", "later_kind"])), "Runs that can still open a hidden folder until they end: research job, another CrazyAgent run.", "a kind this app does not know is still named");
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const start = source.indexOf("function HiddenSection(");
  const hidden = source.slice(start, source.indexOf("\n}\n", start));
  assert.match(hidden, /\{!fullAccess && hiddenRunsLine\(latest\) !== null && <p className="pj-note warn" role="status">\{hiddenRunsLine\(latest\)\}<\/p>\}/, "Full access hides nothing, so it lists no run");
  assert.equal(settings.HIDDEN_RUNS_POLL_MS, 3000);
  assert.match(hidden, /if \(!listed\) return;/, "it reads again only while a run is listed");
  assert.match(hidden, /setInterval\(/, "a run that ended leaves the page without a reload");
  assert.match(hidden, /setLatest\(result\.value\)/);
});
