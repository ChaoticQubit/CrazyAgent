import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { withPage } from "../browser.mjs";
import { approvedBrand, dockIcon, iconsVerdict, served } from "./trial4-icons.mjs";
import { checks, faviconReadings } from "./trial4/n-d33-icons.mjs";

const APP = fileURLToPath(new URL("../../", import.meta.url));
const BRAND = fileURLToPath(new URL("../../src/assets/brand/", import.meta.url));
const APP_ICON = fileURLToPath(new URL("../../src-tauri/icons/icon.icns", import.meta.url));
const CSP = JSON.parse(readFileSync(join(APP, "src-tauri", "tauri.conf.json"), "utf8")).app.security.csp;
const LOAD_TRIES = 100;
const LOAD_POLL_MS = 50;

/** A brand file as a route the loopback site serves. Inputs: the file and its media type.
 * Output: [type, bytes]. */
function brand(file, type) {
  return [type, readFileSync(join(BRAND, file))];
}

/** Serves pages on loopback with the app's own CSP header, as the app window gets them.
 * Input: routes { path: [type, bytes] }; any other path answers 404. Output: { origin, close }. */
async function serve(routes) {
  const server = createServer((request, reply) => {
    const route = routes[new URL(request.url, "http://loopback").pathname];
    reply.writeHead(route ? 200 : 404, { "Content-Security-Policy": CSP, "Content-Type": route?.[0] ?? "text/plain" });
    reply.end(route?.[1] ?? "");
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((resolve) => server.close(resolve)) };
}

/** Opens the app's own index.html in headless Chrome under the app's CSP and reads its
 * favicons with the page function trial check N-D33 runs. Input: the routes served beside
 * index.html. Output: the page's readings. */
async function readInChrome(routes) {
  const site = await serve({ "/": ["text/html", readFileSync(join(APP, "index.html"))], ...routes });
  try {
    return await withPage(async ({ evaluate, send }) => {
      await send("Page.navigate", { url: `${site.origin}/` });
      for (let tries = 0; await evaluate(`document.readyState === "complete" && location.origin === ${JSON.stringify(site.origin)}`) !== true; tries += 1) {
        assert(tries < LOAD_TRIES, "the page loaded");
        await pause(LOAD_POLL_MS);
      }
      const approved = approvedBrand().favicons.map(({ label, url }) => ({ label, url }));
      return evaluate(`new Promise((done) => (${faviconReadings})(${JSON.stringify(approved)}, done))`);
    });
  } finally {
    await site.close();
  }
}

/** A drawn image of one pixel. Input: its RGBA values. Output: { width, height, pixels }. */
function pixel(pixels) {
  return { width: 1, height: 1, pixels };
}

/** Page readings with the approved favicons drawn as single pixels. Inputs: the links and,
 * optionally, other approved drawings. Output: { links, approved }. */
function drawn(links, approved = [{ label: "SVG", image: pixel([1, 2, 3, 4]) }, { label: "PNG", image: pixel([5, 6, 7, 8]) }]) {
  return { links, approved };
}

/** The SHA-256 of a file's bytes. Input: the path. Output: the hex digest. */
function sha(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** Builds a scratch .app bundle whose Info.plist names a Dock icon. Inputs: the icon name the
 * plist gives and the Resources files { name: source path }. Output: { root, bundle }. */
function bundle(iconFile, resources) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-t4i-")));
  const app = join(root, "CrazyAgent.app");
  mkdirSync(join(app, "Contents", "Resources"), { recursive: true });
  mkdirSync(join(app, "Contents", "MacOS"));
  writeFileSync(join(app, "Contents", "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict><key>CFBundleIconFile</key><string>${iconFile}</string></dict></plist>\n`);
  for (const [name, source] of Object.entries(resources)) copyFileSync(source, join(app, "Contents", "Resources", name));
  return { root, bundle: app };
}

/** A passing reading of the window, built from the approved brand. Input: the approved brand
 * and the fields to change. Output: the verdict input. */
function reading(approved, fields = {}) {
  const [, reduced] = approved.marks;
  return {
    home: 9, project: 4, sidebar: 7,
    glyphs: [{ coil: reduced.coil, squares: reduced.squares }],
    favicons: approved.favicons.map(({ label, file }) => ({ label, href: `/assets/${file}`, problem: null })),
    dock: { bundle: "/Applications/CrazyAgent.app", file: "icon.icns", sha: approved.app },
    approved, ...fields,
  };
}

test("N-D33 is one trial 4 check for OP-59 that runs at the end", () => {
  assert.equal(checks.length, 1);
  const [check] = checks;
  assert.deepEqual([check.id, check.op, check.after], ["N-D33", ["OP-59"], "end"]);
  assert.ok(check.why.startsWith("owner: "));
  assert.equal(typeof check.run, "function");
});

test("the approved brand is read from the redesign's own files: both mark drawings, both favicons and the app icon", () => {
  const approved = approvedBrand();
  assert.deepEqual(approved.marks.map((mark) => [mark.name, mark.coil, mark.squares.length]),
    [["full", "M59 5H5V59H59V15H15V49H49V22", 4], ["reduced", "M15 1H1V15H15V4H4V13", 2]]);
  assert.equal(approved.marks[0].squares[0], "21.85 25.05 9.5 9.5 rotate(24 26.6 29.8)");
  assert.deepEqual(approved.favicons.map((icon) => [icon.label, icon.file, icon.url]), [
    ["SVG", "favicon.svg", `data:image/svg+xml;base64,${readFileSync(join(BRAND, "favicon.svg")).toString("base64")}`],
    ["PNG", "favicon-32.png", `data:image/png;base64,${readFileSync(join(BRAND, "favicon-32.png")).toString("base64")}`],
  ]);
  assert.equal(approved.app, sha(APP_ICON));
});

test("served matches each approved favicon to the link of its file name, hashed by the build or not, and to the pixels it draws", () => {
  const approved = approvedBrand();
  assert.deepEqual(served(drawn([
    { href: "/assets/favicon-32-DxJSZz3f.png", image: pixel([5, 6, 7, 8]) },
    { href: "/src/assets/brand/favicon.svg", image: pixel([1, 2, 3, 4]) },
  ]), approved), [
    { label: "SVG", href: "/src/assets/brand/favicon.svg", problem: null },
    { label: "PNG", href: "/assets/favicon-32-DxJSZz3f.png", problem: null },
  ]);
  assert.deepEqual(served(drawn([{ href: "/assets/favicon-BGMZhkR9.svg?v=1", image: pixel([1, 2, 3, 4]) }]), approved)[0],
    { label: "SVG", href: "/assets/favicon-BGMZhkR9.svg?v=1", problem: null });
});

test("served names each approved favicon the window does not link, cannot draw or draws differently", () => {
  const approved = approvedBrand();
  const problems = (links, reference) => served(drawn(links, reference), approved).map((icon) => icon.problem);
  const none = ["the window links no SVG favicon (favicon.svg)", "the window links no PNG favicon (favicon-32.png)"];
  assert.deepEqual(problems([]), none);
  assert.deepEqual(problems([
    { href: "/assets/favicon-32-DxJSZz3f.svg", image: pixel([1, 2, 3, 4]) },
    { href: "/assets/favicon-x.png", image: pixel([5, 6, 7, 8]) },
  ]), none);
  assert.deepEqual(problems([
    { href: "/assets/favicon-BGMZhkR9.svg", image: pixel([1, 2, 3, 5]) },
    { href: "/assets/favicon-32-DxJSZz3f.png", image: { failed: "EncodingError: The source image cannot be decoded." } },
  ]), [
    "the window's SVG favicon /assets/favicon-BGMZhkR9.svg is not the approved favicon.svg",
    "the window could not draw its PNG favicon /assets/favicon-32-DxJSZz3f.png: EncodingError: The source image cannot be decoded.",
  ]);
  assert.deepEqual(problems([
    { href: "/assets/favicon-BGMZhkR9.svg", image: { width: 2, height: 1, pixels: [1, 2, 3, 4, 1, 2, 3, 4] } },
    { href: "/assets/favicon-32-DxJSZz3f.png", image: pixel([5, 6, 7, 8]) },
  ]), ["the window's SVG favicon /assets/favicon-BGMZhkR9.svg is not the approved favicon.svg", null]);
  assert.deepEqual(problems([
    { href: "/assets/favicon-BGMZhkR9.svg", image: pixel([1, 2, 3, 4]) },
    { href: "/assets/favicon-32-DxJSZz3f.png", image: pixel([5, 6, 7, 8]) },
  ], [{ label: "SVG", image: { failed: "SecurityError: The canvas has been tainted." } }, { label: "PNG", image: pixel([5, 6, 7, 8]) }]),
  ["the window could not draw the approved SVG favicon: SecurityError: The canvas has been tainted.", null]);
  assert.throws(() => served(drawn([{ href: "/assets/favicon-BGMZhkR9.svg", image: { width: 1, height: 1, pixels: [1, 2, 3] } }]), approved), /a drawn image is its width, height and RGBA pixels/u);
  assert.throws(() => served(drawn([], [{ label: "SVG", image: pixel([1, 2, 3, 4]) }]), approved), /the readings hold each approved favicon/u);
  assert.throws(() => served({ failed: "TypeError: Load failed" }, approved), /the favicon readings list the links and the approved favicons/u);
});

test("under the app's own CSP the window's favicons are read from the images it draws and are the approved ones", async () => {
  const answer = await readInChrome({
    "/src/assets/brand/favicon-32.png": brand("favicon-32.png", "image/png"),
    "/src/assets/brand/favicon.svg": brand("favicon.svg", "image/svg+xml"),
  });
  assert.equal(answer.failed, undefined, "the page reads its favicons under the app's CSP");
  assert.deepEqual(answer.links.map((link) => link.href), ["/src/assets/brand/favicon-32.png", "/src/assets/brand/favicon.svg"]);
  assert.deepEqual(served(answer, approvedBrand()), [
    { label: "SVG", href: "/src/assets/brand/favicon.svg", problem: null },
    { label: "PNG", href: "/src/assets/brand/favicon-32.png", problem: null },
  ]);
});

test("under the app's own CSP a favicon that draws another image or does not load is named", async () => {
  const answer = await readInChrome({ "/src/assets/brand/favicon.svg": brand("mark.svg", "image/svg+xml") });
  assert.equal(answer.failed, undefined, "the page reads its favicons under the app's CSP");
  const [svg, png] = served(answer, approvedBrand()).map((icon) => icon.problem);
  assert.equal(svg, "the window's SVG favicon /src/assets/brand/favicon.svg is not the approved favicon.svg");
  assert.match(png, /^the window could not draw its PNG favicon \/src\/assets\/brand\/favicon-32\.png: \w+/u);
});

test("dockIcon reads the icon the bundle's Info.plist names, as the Dock does", () => {
  const plain = bundle("icon", { "icon.icns": APP_ICON });
  const other = bundle("other.icns", { "other.icns": join(BRAND, "favicon-32.png") });
  try {
    assert.deepEqual(dockIcon(join(plain.bundle, "Contents", "MacOS", "crazyagent-desktop")), { bundle: plain.bundle, file: "icon.icns", sha: sha(APP_ICON) });
    assert.deepEqual(dockIcon(other.bundle), { bundle: other.bundle, file: "other.icns", sha: sha(join(BRAND, "favicon-32.png")) });
  } finally {
    rmSync(plain.root, { recursive: true, force: true });
    rmSync(other.root, { recursive: true, force: true });
  }
});

test("dockIcon fails plainly for a bare binary, a missing icon file or a name outside Resources", () => {
  const missing = bundle("gone", {});
  const outside = bundle("../../Info", {});
  try {
    assert.throws(() => dockIcon("/repo/target/debug/crazyagent-desktop"), /^Error: OP-59: the Dock icon is read from the app's \.app bundle; the trial ran \/repo\/target\/debug\/crazyagent-desktop$/u);
    assert.throws(() => dockIcon(missing.bundle), /^Error: OP-59: .*CrazyAgent\.app has no Contents\/Resources\/gone\.icns for its Dock icon$/u);
    assert.throws(() => dockIcon(outside.bundle), /^Error: OP-59: .*CrazyAgent\.app names a Dock icon outside Resources: \.\.\/\.\.\/Info$/u);
  } finally {
    rmSync(missing.root, { recursive: true, force: true });
    rmSync(outside.root, { recursive: true, force: true });
  }
});

test("the icons check passes when Home, the project page and the sidebar draw icons and the brand is the approved one", () => {
  const approved = approvedBrand();
  assert.equal(iconsVerdict(reading(approved)),
    "Home draws 9 icons, the project's Context page 4 and the sidebar 7; the sidebar shows the approved Ember 4 mark (reduced drawing); the window serves the approved SVG and PNG favicons; the Dock icon is /Applications/CrazyAgent.app icon.icns, the approved app icon");
  const [full] = approved.marks;
  assert.match(iconsVerdict(reading(approved, { glyphs: [{ coil: full.coil, squares: full.squares }] })), /approved Ember 4 mark \(full drawing\)/u);
});

test("the icons check fails in the owner's words for each missing icon", () => {
  const approved = approvedBrand();
  const [, reduced] = approved.marks;
  const fails = (fields, pattern) => assert.throws(() => iconsVerdict(reading(approved, fields)), pattern);
  fails({ home: 0 }, /^Error: OP-59: Home draws no icons$/u);
  fails({ project: 0 }, /^Error: OP-59: the project's Context page draws no icons$/u);
  fails({ sidebar: 0 }, /^Error: OP-59: the sidebar draws no icons$/u);
  fails({ glyphs: [] }, /^Error: OP-59: the sidebar shows no CrazyAgent mark$/u);
  fails({ glyphs: [{ coil: reduced.coil, squares: reduced.squares }, { coil: reduced.coil, squares: reduced.squares }] }, /^Error: OP-59: the sidebar shows 2 drawings of the CrazyAgent mark at once$/u);
  fails({ glyphs: [{ coil: "M2 2H14V14H2Z", squares: [] }] }, /^Error: OP-59: the sidebar's CrazyAgent mark is not the approved Ember 4 mark: M2 2H14V14H2Z with 0 squares$/u);
  fails({ glyphs: [{ coil: reduced.coil, squares: reduced.squares.slice(1) }] }, /is not the approved Ember 4 mark: M15 1H1V15H15V4H4V13 with 1 squares$/u);
  fails({ favicons: [{ label: "SVG", href: null, problem: "the window links no SVG favicon (favicon.svg)" }, { label: "PNG", href: "/assets/favicon-32.png", problem: null }] }, /^Error: OP-59: the window links no SVG favicon \(favicon\.svg\)$/u);
  fails({ favicons: [{ label: "SVG", href: "/assets/favicon.svg", problem: null }, { label: "PNG", href: "/assets/favicon-32.png", problem: "the window's PNG favicon /assets/favicon-32.png is not the approved favicon-32.png" }] }, /^Error: OP-59: the window's PNG favicon \/assets\/favicon-32\.png is not the approved favicon-32\.png$/u);
  assert.throws(() => iconsVerdict(reading(approved, { favicons: [] })), /one judgement per approved favicon/u);
  fails({ dock: { bundle: "/Applications/CrazyAgent.app", file: "icon.icns", sha: "0".repeat(64) } }, /^Error: OP-59: the Dock icon \/Applications\/CrazyAgent\.app icon\.icns is not the approved app icon$/u);
  assert.throws(() => iconsVerdict(reading(approved, { home: -1 })), /counts are whole numbers/u);
});
