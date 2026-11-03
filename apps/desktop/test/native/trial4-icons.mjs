import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { appBundle } from "./trial-app.mjs";

const BRAND = fileURLToPath(new URL("../../src/assets/brand/", import.meta.url));
const APP_ICON = fileURLToPath(new URL("../../src-tauri/icons/icon.icns", import.meta.url));
const MARKS = [["full", "mark.svg"], ["reduced", "mark-reduced.svg"]];
const FAVICONS = [["SVG", "favicon.svg"], ["PNG", "favicon-32.png"]];
const SQUARE = ["x", "y", "width", "height", "transform"];
const PLIST_MS = 10000;

/** The SHA-256 of some bytes. Input: a buffer. Output: the hex digest. */
function digest(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

/** Reads the geometry of one approved mark file: its coil path and each square as
 * "x y width height transform", the form the sidebar's drawing is read in. Input: the SVG
 * text. Output: { coil, squares }. */
function geometry(svg) {
  const coil = /<path\b[^>]*\bd="([^"]+)"/u.exec(svg)?.[1];
  assert(coil, "an approved mark draws its coil as one path");
  const squares = [...svg.matchAll(/<rect\b[^>]*>/gu)].map(([rect]) => SQUARE.map((name) => new RegExp(`\\b${name}="([^"]+)"`, "u").exec(rect)?.[1]).join(" "));
  assert(squares.length > 0 && squares.every((square) => square.split(" ").length >= SQUARE.length), "an approved mark has squares with their full geometry");
  return { coil, squares };
}

/** The approved brand redesign, read from its own files in the repository: the full and
 * reduced Ember 4 mark drawings, the SVG and PNG favicons and the app icon the bundle ships.
 * Output: { marks: [{ name, coil, squares }], favicons: [{ label, sha }], app }. */
export function approvedBrand() {
  return {
    marks: MARKS.map(([name, file]) => ({ name, ...geometry(readFileSync(join(BRAND, file), "utf8")) })),
    favicons: FAVICONS.map(([label, file]) => ({ label, sha: digest(readFileSync(join(BRAND, file))) })),
    app: digest(readFileSync(APP_ICON)),
  };
}

/** Hashes each favicon the window fetched from its own link rel=icon. Input: the page's
 * readings [{ href, status, size, bytes }], bytes being empty when the file was too big to
 * return. Output: [{ href, sha }], sha null for a failed or oversized fetch. */
export function served(readings) {
  assert(Array.isArray(readings), "the favicon readings are a list");
  return readings.map(({ href, status, size, bytes }) => {
    assert(typeof href === "string" && Number.isInteger(status) && Number.isInteger(size) && Array.isArray(bytes), "a favicon reading has its href, status, size and bytes");
    assert(bytes.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255), "favicon bytes are 0 to 255");
    const whole = status === 200 && bytes.length === size && size > 0;
    return { href, sha: whole ? digest(Buffer.from(bytes)) : null };
  });
}

/** The icon the Dock shows for the trial's app: the file its bundle's Info.plist names in
 * CFBundleIconFile, read from Contents/Resources. Input: the app, its .app bundle or the
 * binary inside it. Output: { bundle, file, sha }; throws with OP-59 when there is none. */
export function dockIcon(app) {
  let bundle;
  try {
    bundle = appBundle(app);
  } catch {
    throw new Error(`OP-59: the Dock icon is read from the app's .app bundle; the trial ran ${app}`);
  }
  const plist = spawnSync("/usr/bin/plutil", ["-extract", "CFBundleIconFile", "raw", "-o", "-", join(bundle, "Contents", "Info.plist")], { encoding: "utf8", timeout: PLIST_MS, killSignal: "SIGKILL" });
  const named = plist.status === 0 ? plist.stdout.trim() : "";
  if (!named) throw new Error(`OP-59: ${bundle} names no Dock icon in CFBundleIconFile`);
  if (named.includes("/") || named.startsWith(".")) throw new Error(`OP-59: ${bundle} names a Dock icon outside Resources: ${named}`);
  const file = named.endsWith(".icns") ? named : `${named}.icns`;
  const path = join(bundle, "Contents", "Resources", file);
  if (!existsSync(path)) throw new Error(`OP-59: ${bundle} has no Contents/Resources/${file} for its Dock icon`);
  return { bundle, file, sha: digest(readFileSync(path)) };
}

/** Judges trial 4 check N-D33 on what the owner sees. Input: { home, project, sidebar,
 * glyphs, favicons, dock, approved }: the icons Home, the project's Context page and the
 * sidebar draw, the sidebar mark drawings shown, the served favicons, the Dock icon and the
 * approved brand. Output: the step detail. Throws with OP-59 for each missing icon. */
export function iconsVerdict({ home, project, sidebar, glyphs, favicons, dock, approved }) {
  assert([home, project, sidebar].every((count) => Number.isInteger(count) && count >= 0), "the icon counts are whole numbers");
  assert(Array.isArray(glyphs) && Array.isArray(favicons) && typeof dock?.sha === "string", "the readings are lists and a Dock icon");
  if (home === 0) throw new Error("OP-59: Home draws no icons");
  if (project === 0) throw new Error("OP-59: the project's Context page draws no icons");
  if (sidebar === 0) throw new Error("OP-59: the sidebar draws no icons");
  if (glyphs.length === 0) throw new Error("OP-59: the sidebar shows no CrazyAgent mark");
  if (glyphs.length > 1) throw new Error(`OP-59: the sidebar shows ${glyphs.length} drawings of the CrazyAgent mark at once`);
  const [glyph] = glyphs;
  const mark = approved.marks.find((one) => one.coil === glyph.coil && JSON.stringify(one.squares) === JSON.stringify(glyph.squares));
  if (!mark) throw new Error(`OP-59: the sidebar's CrazyAgent mark is not the approved Ember 4 mark: ${glyph.coil} with ${glyph.squares.length} squares`);
  const shas = new Set(favicons.map((icon) => icon.sha).filter(Boolean));
  const missing = approved.favicons.find((icon) => !shas.has(icon.sha));
  if (missing) throw new Error(`OP-59: the window does not serve the approved ${missing.label} favicon`);
  if (dock.sha !== approved.app) throw new Error(`OP-59: the Dock icon ${dock.bundle} ${dock.file} is not the approved app icon`);
  return `Home draws ${home} icons, the project's Context page ${project} and the sidebar ${sidebar}; the sidebar shows the approved Ember 4 mark (${mark.name} drawing); the window serves the approved ${approved.favicons.map((icon) => icon.label).join(" and ")} favicons; the Dock icon is ${dock.bundle} ${dock.file}, the approved app icon`;
}
