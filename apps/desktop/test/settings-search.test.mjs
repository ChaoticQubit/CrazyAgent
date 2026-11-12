import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-app-plugins.ts");
const screen = await import("../src/screens/settings/search.tsx");

const SIZE = 125729604;
const LOCAL = { kind: "local", runtime: "static-retrieval-mrl-en-v1", download_bytes: SIZE };
const LOOPBACK = { kind: "http", url: "http://LocalHost:11434/api/embed", hosts: ["localhost"], local_unencrypted: true, dimensions: 768 };
const HTTPS = { kind: "http", url: "https://api.voyageai.com/v1/embeddings", hosts: ["api.voyageai.com"], local_unencrypted: false, dimensions: 256 };
const SENDS = ["Session text", "Memories"];
const OPTIONS = [
  { name: "local", title: "Built-in model", runs: LOCAL, usable: true, reason: null, sends: [] },
  { name: "dock-embed", title: "Dock embeddings", runs: LOOPBACK, usable: true, reason: null, sends: SENDS },
  { name: "voyage", title: "Voyage embeddings", runs: HTTPS, usable: false, reason: "Set its key in App plugins first.", sends: SENDS },
];
const view = (patch = {}) => ({
  version: 4, choice: { kind: "plugin", name: "local" }, options: OPTIONS, note: null, model: { state: "ready", size_bytes: SIZE }, progress: { embedded: 120, total: 480 }, ...patch,
});
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const actions = new Proxy({}, { get: () => forbidden });
const render = (patch = {}) => renderToStaticMarkup(createElement(screen.SearchSettingsView, {
  search: { kind: "ok", value: view() }, dialog: { kind: "none" }, busy: false, failure: "", actions, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const radios = (markup) => [...markup.matchAll(/<input type="radio" name="search-choice" value="([^"]+)"([^>]*)>/gu)].map((match) => ({
  value: match[1], checked: /checked=""/u.test(match[2]), disabled: /disabled=""/u.test(match[2]),
}));

test("Search (S1, S2, S10): Full text only and every embedding plugin are offered, the saved one checked, an unusable one off with its reason", () => {
  assert.deepEqual(radios(render()), [
    { value: "full_text", checked: false, disabled: false },
    { value: "plugin:local", checked: true, disabled: false },
    { value: "plugin:dock-embed", checked: false, disabled: false },
    { value: "plugin:voyage", checked: false, disabled: true },
  ]);
  assert.deepEqual(radios(render({ search: { kind: "ok", value: view({ choice: { kind: "full_text" } }) } })).filter((radio) => radio.checked).map((radio) => radio.value), ["full_text"]);
  const shown = text(render());
  for (const words of ["Full text only", "Built-in model", "Dock embeddings", "Voyage embeddings", "Set its key in App plugins first."]) assert.ok(shown.includes(words), `Search shows ${words}`);
  assert.deepEqual(api.searchChoice("full_text"), { kind: "full_text" });
  assert.deepEqual(api.searchChoice("plugin:dock-embed"), { kind: "plugin", name: "dock-embed" });
  assert.equal(api.choiceValue({ kind: "plugin", name: "local" }), "plugin:local");
  assert.equal(api.choiceValue({ kind: "full_text" }), "full_text");
});

test("Search (S3, S4): choosing an HTTP plugin asks first, naming its host and the engine's own list of what leaves this computer", () => {
  assert.equal(api.hostOf("http://LocalHost:11434/api/embed"), "localhost");
  assert.equal(api.hostOf("https://api.voyageai.com/v1/embeddings"), "api.voyageai.com");
  assert.equal(api.hostOf("not a url"), null);
  assert.equal(api.httpOption(OPTIONS[1]), true);
  assert.equal(api.httpOption(OPTIONS[0]), false);
  const markup = render({ dialog: { kind: "http", option: OPTIONS[1] } });
  const shown = text(markup);
  assert.ok(shown.includes("Send search text to localhost?"));
  assert.ok(shown.includes("Once chosen, CrazyAgent sends these to localhost in the background:"));
  for (const line of SENDS) assert.ok(shown.includes(line), `the confirm lists ${line}`);
  assert.ok(!shown.includes("Tool inputs and outputs"), "the list is the engine's, not a copy in the page");
  assert.ok(shown.includes("Send to localhost") && shown.includes("Cancel"));
});

test("Search (S5, S6): a model not downloaded shows its size and Download, and the confirm names the size", () => {
  const before = text(render({ search: { kind: "ok", value: view({ model: { state: "not_downloaded", size_bytes: SIZE } }) } }));
  assert.ok(before.includes("Not on this computer. A 126 MB download, once; it then works offline."));
  assert.ok(before.includes("Download"));
  const asked = text(render({ search: { kind: "ok", value: view({ model: { state: "not_downloaded", size_bytes: SIZE } }) }, dialog: { kind: "download", bytes: SIZE } }));
  assert.ok(asked.includes("Download the built-in model?"));
  assert.ok(asked.includes("126 MB, once. CrazyAgent checks it before use; search uses full text until it is ready."));
  assert.ok(asked.includes("Cancel"));
  const held = text(render({ search: { kind: "ok", value: view({ model: { state: "not_downloaded", size_bytes: SIZE } }) }, dialog: { kind: "download", bytes: 2000000 } }));
  assert.ok(held.includes("2 MB, once."), "the confirm names the bytes it sends as confirm_bytes, held from when it opened (PUD-M2)");
  assert.equal(api.megabytes(SIZE), "126 MB");
  assert.equal(api.megabytes(0), "0 MB");
});

test("Search (S7): a running download shows how far it got and Cancel download", () => {
  const markup = render({ search: { kind: "ok", value: view({ model: { state: "downloading", done_bytes: 63000000, size_bytes: SIZE } }) } });
  assert.ok(text(markup).includes("Downloading: 63 MB of 126 MB"));
  assert.match(markup, /<progress[^>]*value="63000000"[^>]*max="125729604"|<progress[^>]*max="125729604"[^>]*value="63000000"/u);
  assert.ok(text(markup).includes("Cancel download"));
});

test("Search (S8, S9): a failed download shows its problem and Download again; a ready model offers Delete model with a confirm", () => {
  const problem = { code: "search_model_hash_mismatch", line: null, field: null, message: "The download didn't match its checksum." };
  const failed = text(render({ search: { kind: "ok", value: view({ model: { state: "failed", problem, size_bytes: SIZE } }) } }));
  assert.ok(failed.includes("The download didn't match its checksum. (search_model_hash_mismatch)"));
  assert.ok(failed.includes("Download again"));
  const ready = text(render());
  assert.ok(ready.includes("Ready: 126 MB on this computer."));
  assert.ok(ready.includes("Delete model"));
  const asked = text(render({ dialog: { kind: "delete" } }));
  assert.ok(asked.includes("Delete the built-in model?"));
  assert.ok(asked.includes("Frees 126 MB. Search with the built-in model stops until you download it again."));
});

test("Search (S11, S12): embedding progress shows for a plugin choice, and a failure line shows above the choices", () => {
  assert.ok(text(render()).includes("Embedded 120 of 480 sessions and memories."));
  assert.ok(!text(render({ search: { kind: "ok", value: view({ choice: { kind: "full_text" } }) } })).includes("Embedded"));
  assert.ok(text(render({ failure: api.SEARCH_STALE })).includes("Search settings changed in another window. This page now shows the saved ones; choose again."));
});

test("Search: loading, not ready and a refused read each keep the title", () => {
  assert.ok(text(render({ search: { kind: "loading" } })).includes("Search"));
  assert.ok(text(render({ search: { kind: "not_ready" } })).includes("isn't ready"));
  const refused = text(render({ search: { kind: "error", code: "search_unavailable", message: "x" } }));
  assert.ok(refused.includes("Try again"));
});

test("Search: each action is a direct bridge call that names the host the confirm showed", () => {
  const file = readFileSync(new URL("../src/api-app-plugins.ts", import.meta.url), "utf8");
  assert.match(file, /call<SearchView>\("desktop_search_settings"\)/u);
  assert.match(file, /call<SearchSave>\("desktop_set_search", \{ choice, confirmHost, version \}\)/u);
  for (const command of ["desktop_search_model_download", "desktop_search_model_cancel", "desktop_search_model_delete"]) {
    assert.match(file, new RegExp(`call<SearchView>\\("${command}"\\)`, "u"));
  }
  const page = readFileSync(new URL("../src/screens/settings/search.tsx", import.meta.url), "utf8");
  assert.match(page, /const host = hostOf\(option\.runs\.url\);/u, "the confirmed host is the one the dialog named");
  assert.match(page, /saveSearch\(\{ kind: "plugin", name: option\.name \}, host, version\)/u);
});

test("Search (PUD-M2): the download start carries confirm_bytes, the size its confirm named", () => {
  const file = readFileSync(new URL("../src/api-app-plugins.ts", import.meta.url), "utf8");
  assert.match(file, /export function downloadSearchModel\(confirmBytes: number\): Promise<Result<SearchView>> \{\n  return call<SearchView>\("desktop_search_model_download", \{ confirmBytes \}\);/u);
  const page = readFileSync(new URL("../src/screens/settings/search.tsx", import.meta.url), "utf8");
  assert.match(page, /setDialog\(\{ kind: "download", bytes: search\.value\.model\.size_bytes \}\)/u);
  assert.match(page, /const bytes = dialog\.bytes;\n\s*void model\(\(\) => downloadSearchModel\(bytes\)\);/u);
  const bridge = readFileSync(new URL("../src-tauri/src/bridge/app_plugins.rs", import.meta.url), "utf8");
  assert.match(bridge, /pub fn search_model_download\(\s*&self,\s*confirm_bytes: u64,?\s*\) -> Result<Unlocked<SearchView>, BridgeError>/u);
  assert.match(bridge, /\.search_model_download\(confirm_bytes\)/u);
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(lib, /async fn desktop_search_model_download\(\s*webview: Webview,\s*state: State<'_, NativeState>,\s*confirm_bytes: u64,\s*\)/u);
  assert.match(lib, /\.search_model_download\(confirm_bytes\)\?;/u);
});

test("Search (PUD-M1): the page reads again while the model downloads or a usable chosen plugin is still embedding, and stops once nothing moves", () => {
  assert.equal(typeof api.searchPolling, "function", "searchPolling is exported");
  assert.equal(typeof api.SEARCH_POLL_MS, "number", "SEARCH_POLL_MS is exported");
  assert.ok(api.SEARCH_POLL_MS >= 1000 && api.SEARCH_POLL_MS <= 5000, "a read every 1 to 5 seconds");
  const downloading = { state: "downloading", done_bytes: 1000, size_bytes: SIZE };
  assert.equal(api.searchPolling(view({ model: downloading, choice: { kind: "full_text" } })), true, "a running download moves");
  assert.equal(api.searchPolling(view()), true, "120 of 480 embedded with the usable built-in model chosen");
  assert.equal(api.searchPolling(view({ progress: { embedded: 480, total: 480 } })), false, "all embedded, model ready");
  assert.equal(api.searchPolling(view({ choice: { kind: "full_text" } })), false, "full text has no embedding to watch");
  assert.equal(api.searchPolling(view({ choice: { kind: "plugin", name: "voyage" } })), false, "an unusable plugin embeds nothing");
  assert.equal(api.searchPolling(view({ model: { state: "failed", size_bytes: SIZE, problem: { code: "x", line: null, field: null, message: "m" } }, choice: { kind: "full_text" } })), false);
  const page = readFileSync(new URL("../src/screens/settings/search.tsx", import.meta.url), "utf8");
  assert.match(page, /const polling = search\.kind === "ok" && searchPolling\(search\.value\);/u);
  assert.match(page, /useEffect\(\(\) => \{\n    if \(!polling\) return;\n    const timer = window\.setInterval\(reload, SEARCH_POLL_MS\);\n    return \(\) => window\.clearInterval\(timer\);\n  \}, \[polling\]\);/u);
});

test("Search (74e0d0a36): the note after a removed plugin shows above the choices, and a gone or refused chosen plugin stays listed, checked and off with its reason", () => {
  const NOTE = "Your chosen plugin was removed, so search uses full text.";
  const noted = text(render({ search: { kind: "ok", value: view({ choice: { kind: "full_text" }, note: NOTE }) } }));
  assert.ok(noted.includes(NOTE), "the engine's note is shown");
  assert.ok(noted.indexOf(NOTE) < noted.indexOf("Full text only"), "above the choices");
  assert.ok(!text(render()).includes(NOTE));
  const gone = { name: "harbor", title: "Harbor embeddings", runs: null, usable: false, reason: "This plugin is gone from your plugins folder.", sends: [] };
  assert.equal(api.httpOption(gone), false, "a plugin with no runs is never an HTTP send");
  const markup = render({ search: { kind: "ok", value: view({ choice: { kind: "plugin", name: "harbor" }, options: [...OPTIONS, gone] }) } });
  assert.deepEqual(radios(markup).find((radio) => radio.value === "plugin:harbor"), { value: "plugin:harbor", checked: true, disabled: true });
  assert.ok(text(markup).includes("This plugin is gone from your plugins folder."));
  assert.ok(!text(markup).includes("Reaches undefined"));
});
