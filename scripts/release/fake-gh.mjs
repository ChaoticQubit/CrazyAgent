import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

const STATE = process.env.FAKE_STATE;
const REPO = "repos/example/fake-repo";
const VALUE_FLAGS = new Set(["--repo", "--target", "--title", "--notes-file", "--dir", "--pattern", "--jq", "-X", "-f", "-F"]);

/**
 * Reads one JSON file of the stand-in state. Inputs: the file name and the value to use when
 * it is absent. Output: the parsed value.
 */
function load(name, fallback) {
  const file = join(STATE, name);
  return existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : fallback;
}

/** Writes one JSON file of the stand-in state. Inputs: the file name and the value. */
function save(name, value) {
  writeFileSync(join(STATE, name), `${JSON.stringify(value)}\n`);
}

/** Ends the stand-in as gh ends on an error. Inputs: the message for stderr and the exit status. */
function fail(message, status = 1) {
  process.stderr.write(`${message}\n`);
  process.exit(status);
}

/** Prints `text` and ends the stand-in with status 0. Input: the text. */
function done(text) {
  process.stdout.write(text);
  process.exit(0);
}

/**
 * Runs a `--jq` filter with the real jq, as gh does. Inputs: the filter and the value.
 * Output: none; it ends the stand-in with jq's output and status.
 */
function jq(filter, value) {
  const run = spawnSync("jq", ["-r", filter], { input: JSON.stringify(value), encoding: "utf8" });
  process.stdout.write(run.stdout ?? "");
  process.stderr.write(run.stderr ?? "");
  process.exit(run.status ?? 1);
}

/** The sha256 digest of a stored file, as the releases API names it. Input: the path. Output: "sha256:<hex>". */
function digest(path) {
  return `sha256:${createHash("sha256").update(readFileSync(path)).digest("hex")}`;
}

/**
 * Splits gh's arguments into flag values and positional words. Input: the arguments after the
 * subcommand. Output: { values: flag -> list of values, words: positional words }.
 */
function parse(args) {
  const values = new Map();
  const words = [];
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (VALUE_FLAGS.has(arg)) {
      values.set(arg, [...(values.get(arg) ?? []), args[index + 1]]);
      index += 1;
    } else if (arg.startsWith("-")) {
      values.set(arg, [...(values.get(arg) ?? []), true]);
    } else {
      words.push(arg);
    }
  }
  return { values, words };
}

/**
 * The typed fields of a `gh api` call: `-f` strings and `-F` values, where true and false are
 * booleans and @file reads the file. Input: the parsed arguments. Output: field name -> value.
 */
function fields(values) {
  const out = {};
  for (const [flag, typed] of [["-f", false], ["-F", true]]) {
    for (const pair of values.get(flag) ?? []) {
      const at = pair.indexOf("=");
      const name = pair.slice(0, at);
      let value = pair.slice(at + 1);
      if (typed && (value === "true" || value === "false")) value = value === "true";
      else if (typed && value.startsWith("@")) value = readFileSync(value.slice(1), "utf8");
      out[name] = value;
    }
  }
  return out;
}

/**
 * Prints a list as gh api prints it: page one alone without --paginate, the pages run together
 * with --paginate, and an array of pages with --paginate --slurp. Inputs: the items and the
 * parsed arguments. Output: the text gh would print.
 */
function pages(items, values) {
  const size = load("config.json", {}).pageSize ?? 100;
  const chunks = [];
  for (let index = 0; index < items.length; index += size) chunks.push(items.slice(index, index + size));
  if (chunks.length === 0) chunks.push([]);
  if (!values.has("--paginate")) return JSON.stringify(chunks[0]);
  if (values.has("--slurp")) return JSON.stringify(chunks);
  return chunks.map((chunk) => JSON.stringify(chunk)).join("");
}

/**
 * Answers `gh api` calls on the fake repository's releases and tags.
 * Input: the arguments after `api`. Output: none; it ends the stand-in.
 */
function api(args) {
  const config = load("config.json", {});
  const { values, words } = parse(args);
  const method = values.get("-X")?.[0] ?? "GET";
  const path = words[0] ?? "";
  const releases = load("releases.json", []);
  const refs = load("refs.json", []);
  if (method === "GET" && path === `${REPO}/releases?per_page=100`) {
    if (config.listFails) fail("gh: Server Error (HTTP 502)");
    done(pages(releases, values));
  }
  if (method === "GET" && path === `${REPO}/git/matching-refs/tags/build-?per_page=100`) {
    if (config.refsFails) fail("gh: Server Error (HTTP 502)");
    done(pages(refs.filter((ref) => ref.ref.startsWith("refs/tags/build-")), values));
  }
  if (method === "GET" && path === `${REPO}/git/matching-refs/tags/dev`) {
    if (config.readFails) fail("gh: Server Error (HTTP 502)");
    const stale = Number(readFileSync(join(STATE, "stale"), "utf8"));
    const shown = stale > 0 ? [{ ref: "refs/tags/dev" }, { ref: "refs/tags/dev-old" }] : [{ ref: "refs/tags/dev-old" }];
    if (stale > 0) writeFileSync(join(STATE, "stale"), `${stale - 1}\n`);
    if (typeof config.answer === "string") done(`${config.answer}\n`);
    jq(values.get("--jq")[0], shown);
  }
  if (method === "DELETE" && path === `${REPO}/git/refs/tags/dev`) fail("gh: Reference does not exist (HTTP 422)");
  const tag = /^repos\/example\/fake-repo\/git\/refs\/tags\/(build-[^/]+)$/.exec(path);
  if (method === "DELETE" && tag) {
    if ((config.deleteFails ?? []).includes(tag[1])) fail("gh: Server Error (HTTP 500)");
    const left = refs.filter((ref) => ref.ref !== `refs/tags/${tag[1]}`);
    if (left.length === refs.length) fail("gh: Reference does not exist (HTTP 422)");
    save("refs.json", left);
    done("");
  }
  if (method === "POST" && path === `${REPO}/releases`) {
    const given = fields(values);
    const id = Math.max(1000, ...releases.map((release) => release.id)) + 1;
    const release = { id, tag_name: given.tag_name, target_commitish: given.target_commitish, name: given.name, body: given.body, draft: given.draft, prerelease: given.prerelease, make_latest: given.make_latest, assets: [] };
    save("releases.json", [...releases, release]);
    jq(values.get("--jq")[0], release);
  }
  const byId = /^repos\/example\/fake-repo\/releases\/(\d+)$/.exec(path);
  const release = byId ? releases.find((item) => item.id === Number(byId[1])) : undefined;
  if (byId && !release) fail("gh: Not Found (HTTP 404)");
  if (byId && method === "PATCH") {
    if (config.patchFails) fail("gh: Server Error (HTTP 502)");
    Object.assign(release, fields(values));
    if (!release.draft && !refs.some((ref) => ref.ref === `refs/tags/${release.tag_name}`)) save("refs.json", [...refs, { ref: `refs/tags/${release.tag_name}` }]);
    save("releases.json", releases);
    jq(values.get("--jq")[0], release);
  }
  if (byId && method === "GET") {
    if (config.reread === "fails") fail("gh: Server Error (HTTP 502)");
    const shown = { ...release };
    if (config.reread === "published") shown.draft = false;
    if (config.reread === "retagged") shown.tag_name = "build-9.9.9-dev.209901010000";
    done(`${JSON.stringify(shown)}\n`);
  }
  if (byId && method === "DELETE") {
    save("releases.json", releases.filter((item) => item !== release));
    rmSync(join(STATE, "assets", String(release.id)), { recursive: true, force: true });
    done("");
  }
  fail(`fake gh: unexpected call: api ${args.join(" ")}`, 2);
}

/**
 * Answers `gh release` calls: the dev pointer's view, delete and create, and a build's upload,
 * download and delete. Input: the arguments after `release`. Output: none; it ends the stand-in.
 */
function release(args) {
  const config = load("config.json", {});
  const [command, ...rest] = args;
  const { values, words } = parse(rest);
  const [tag, ...files] = words;
  const releases = load("releases.json", []);
  if (values.get("--repo")?.[0] !== "example/fake-repo") fail("fake gh: no --repo example/fake-repo", 2);
  if (tag === "dev") {
    if (command === "view" || command === "delete") done("");
    if (command === "create") {
      if (Number(readFileSync(join(STATE, "stale"), "utf8")) > 0) fail("HTTP 422: Validation Failed: tag_name already exists");
      mkdirSync(join(STATE, "dev"), { recursive: true });
      for (const file of files) copyFileSync(file, join(STATE, "dev", basename(file)));
      copyFileSync(values.get("--notes-file")[0], join(STATE, "dev-notes.txt"));
      done("");
    }
  }
  const named = releases.filter((item) => item.tag_name === tag);
  if (command === "upload") {
    if (named.length !== 1) fail(`release not found: ${tag}`);
    const [build] = named;
    const folder = join(STATE, "assets", String(build.id));
    mkdirSync(folder, { recursive: true });
    for (const [index, file] of files.entries()) {
      if (config.uploadFails && index === 1) {
        save("releases.json", releases);
        fail("gh: Server Error (HTTP 502)");
      }
      const name = basename(file);
      if (build.assets.some((asset) => asset.name === name) && !values.has("--clobber")) fail(`asset under the same name already exists: ${name}`);
      copyFileSync(file, join(folder, name));
      build.assets = [...build.assets.filter((asset) => asset.name !== name), { name, size: readFileSync(file).length, state: "uploaded", digest: config.digestless ? null : digest(join(folder, name)) }];
    }
    save("releases.json", releases);
    done("");
  }
  const published = named.filter((item) => item.draft === false);
  if (command === "download") {
    if (published.length !== 1) fail(`release not found: ${tag}`);
    const wanted = values.get("--pattern") ?? [];
    const matched = published[0].assets.filter((asset) => wanted.includes(asset.name));
    if (matched.length === 0) fail("no assets match the file pattern");
    for (const asset of matched) copyFileSync(join(STATE, "assets", String(published[0].id), asset.name), join(values.get("--dir")[0], asset.name));
    done("");
  }
  if (command === "delete" && values.has("--yes") && values.has("--cleanup-tag")) {
    if ((config.deleteFails ?? []).includes(tag)) fail("gh: Server Error (HTTP 500)");
    if (published.length !== 1) fail(`release not found: ${tag}`);
    save("releases.json", releases.filter((item) => item !== published[0]));
    save("refs.json", load("refs.json", []).filter((ref) => ref.ref !== `refs/tags/${tag}`));
    done("");
  }
  fail(`fake gh: unexpected call: release ${args.join(" ")}`, 2);
}

/**
 * A stand-in for gh in the release workflow tests. It holds no token and reaches no network:
 * every answer comes from the JSON state under FAKE_STATE, and each call is appended to its
 * calls file. Input: gh's arguments. Output: none; it ends the process as gh would.
 */
function main(args) {
  appendFileSync(join(STATE, "calls"), `${args.join(" ")}\n`);
  if (args[0] === "api") api(args.slice(1));
  if (args[0] === "release") release(args.slice(1));
  fail(`fake gh: unexpected call: ${args.join(" ")}`, 2);
}

main(process.argv.slice(2));
