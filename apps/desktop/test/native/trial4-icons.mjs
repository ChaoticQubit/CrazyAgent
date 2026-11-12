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
const FAVICONS = [["SVG", "favicon.svg", "image/svg+xml", /^favicon(-[\w-]{8})?\.svg$/u], ["PNG", "favicon-32.png", "image/png", /^favicon-32(-[\w-]{8})?\.png$/u]];
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
 * reduced Ember 4 mark drawings, the SVG and PNG favicons as data URLs the window can draw
 * under its CSP, and the app icon the bundle ships.
 * Output: { marks: [{ name, coil, squares }], favicons: [{ label, file, url }], app }. */
export function approvedBrand() {
  return {
    marks: MARKS.map(([name, file]) => ({ name, ...geometry(readFileSync(join(BRAND, file), "utf8")) })),
    favicons: FAVICONS.map(([label, file, type]) => ({ label, file, url: `data:${type};base64,${readFileSync(join(BRAND, file)).toString("base64")}` })),
    app: digest(readFileSync(APP_ICON)),
  };
}

/** Checks one image the page drew: a positive size and its RGBA pixels, or why it failed.
 * Input: the image. Output: none; an assertion fails on another shape. */
function checkDrawn(image) {
  if (typeof image?.failed === "string") return;
  const { width, height, pixels } = image ?? {};
  assert(Number.isInteger(width) && Number.isInteger(height) && width > 0 && height > 0 && Array.isArray(pixels)
    && pixels.length === width * height * 4 && pixels.every((value) => Number.isInteger(value) && value >= 0 && value <= 255),
  "a drawn image is its width, height and RGBA pixels");
}

/** Tells whether two drawn images are the same picture. Inputs: two drawn images. Output:
 * true when their sizes and every pixel match. */
function samePicture(one, other) {
  return one.width === other.width && one.height === other.height && one.pixels.every((value, index) => value === other.pixels[index]);
}

/** Judges each approved favicon against what the window drew: the link whose file name is
 * the approved file's, hashed by the build or not, must draw the same pixels as the approved
 * file drawn in the same window. Inputs: the page's readings { links: [{ href, image }],
 * approved: [{ label, image }] } and the approved brand. Output: [{ label, href, problem }],
 * problem null when the window draws the approved favicon. */
export function served(readings, approved) {
  assert(Array.isArray(readings?.links) && Array.isArray(readings?.approved), "the favicon readings list the links and the approved favicons");
  for (const { href, image } of readings.links) {
    assert(typeof href === "string", "a favicon link has its href");
    checkDrawn(image);
  }
  readings.approved.forEach(({ image }) => checkDrawn(image));
  return approved.favicons.map(({ label, file }) => {
    const reference = readings.approved.find((one) => one.label === label)?.image;
    assert(reference !== undefined, "the readings hold each approved favicon");
    const name = FAVICONS.find(([one]) => one === label)[3];
    const link = readings.links.find(({ href }) => name.test(href.split(/[?#]/u)[0].split("/").pop()));
    if (link === undefined) return { label, href: null, problem: `the window links no ${label} favicon (${file})` };
    const { href, image } = link;
    if (image.failed !== undefined) return { label, href, problem: `the window could not draw its ${label} favicon ${href}: ${image.failed}` };
    if (reference.failed !== undefined) return { label, href, problem: `the window could not draw the approved ${label} favicon: ${reference.failed}` };
    if (!samePicture(image, reference)) return { label, href, problem: `the window's ${label} favicon ${href} is not the approved ${file}` };
    return { label, href, problem: null };
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
  assert(favicons.length === approved.favicons.length && favicons.every((icon) => icon.problem === null || typeof icon.problem === "string"), "one judgement per approved favicon");
  if (home === 0) throw new Error("OP-59: Home draws no icons");
  if (project === 0) throw new Error("OP-59: the project's Context page draws no icons");
  if (sidebar === 0) throw new Error("OP-59: the sidebar draws no icons");
  if (glyphs.length === 0) throw new Error("OP-59: the sidebar shows no CrazyAgent mark");
  if (glyphs.length > 1) throw new Error(`OP-59: the sidebar shows ${glyphs.length} drawings of the CrazyAgent mark at once`);
  const [glyph] = glyphs;
  const mark = approved.marks.find((one) => one.coil === glyph.coil && JSON.stringify(one.squares) === JSON.stringify(glyph.squares));
  if (!mark) throw new Error(`OP-59: the sidebar's CrazyAgent mark is not the approved Ember 4 mark: ${glyph.coil} with ${glyph.squares.length} squares`);
  const wrong = favicons.find((icon) => icon.problem !== null);
  if (wrong) throw new Error(`OP-59: ${wrong.problem}`);
  if (dock.sha !== approved.app) throw new Error(`OP-59: the Dock icon ${dock.bundle} ${dock.file} is not the approved app icon`);
  return `Home draws ${home} icons, the project's Context page ${project} and the sidebar ${sidebar}; the sidebar shows the approved Ember 4 mark (${mark.name} drawing); the window serves the approved ${approved.favicons.map((icon) => icon.label).join(" and ")} favicons; the Dock icon is ${dock.bundle} ${dock.file}, the approved app icon`;
}
