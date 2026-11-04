import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { approvedBrand, dockIcon, iconsVerdict, served } from "./trial4-icons.mjs";
import { checks } from "./trial4/n-d33-icons.mjs";

const BRAND = fileURLToPath(new URL("../../src/assets/brand/", import.meta.url));
const APP_ICON = fileURLToPath(new URL("../../src-tauri/icons/icon.icns", import.meta.url));

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
    favicons: approved.favicons.map(({ sha: digest }, index) => ({ href: `/assets/favicon-${index}.x`, sha: digest })),
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
  assert.deepEqual(approved.favicons.map((icon) => [icon.label, icon.sha]),
    [["SVG", sha(join(BRAND, "favicon.svg"))], ["PNG", sha(join(BRAND, "favicon-32.png"))]]);
  assert.equal(approved.app, sha(APP_ICON));
});

test("served hashes each favicon the window fetched and gives no digest for a failed or oversized fetch", () => {
  const bytes = [...readFileSync(join(BRAND, "favicon.svg"))];
  assert.deepEqual(served([
    { href: "/assets/favicon-a.svg", status: 200, size: bytes.length, bytes },
    { href: "/assets/favicon-b.png", status: 404, size: 0, bytes: [] },
    { href: "/assets/favicon-c.png", status: 200, size: 70000, bytes: [] },
  ]), [
    { href: "/assets/favicon-a.svg", sha: sha(join(BRAND, "favicon.svg")) },
    { href: "/assets/favicon-b.png", sha: null },
    { href: "/assets/favicon-c.png", sha: null },
  ]);
  assert.throws(() => served([{ href: "/x", status: 200, size: 1, bytes: [256] }]), /bytes are 0 to 255/);
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
  fails({ favicons: [] }, /^Error: OP-59: the window does not serve the approved SVG favicon$/u);
  fails({ favicons: [{ href: "/assets/favicon.svg", sha: approved.favicons[0].sha }, { href: "/assets/favicon-32.png", sha: null }] }, /^Error: OP-59: the window does not serve the approved PNG favicon$/u);
  fails({ dock: { bundle: "/Applications/CrazyAgent.app", file: "icon.icns", sha: "0".repeat(64) } }, /^Error: OP-59: the Dock icon \/Applications\/CrazyAgent\.app icon\.icns is not the approved app icon$/u);
  assert.throws(() => iconsVerdict(reading(approved, { home: -1 })), /counts are whole numbers/u);
});
