import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { lstatSync, readFileSync, readdirSync, realpathSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

export const BUILD_TAG = /^build-(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-dev\.(\d{12})$/;
export const MANIFESTS = Object.freeze(["latest.json", "cli-latest.json", "SHA256SUMS"]);
const KEEP_MIN = 10;
const KEEP_MAX = 50;
const COMMIT = /^[0-9a-f]{40}$/;
const REPO = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const MODES = Object.freeze({
  draft: { draft: true, ownManifests: true, digestOptional: true },
  published: { draft: false, ownManifests: true, digestOptional: false },
  reused: { draft: false, ownManifests: false, digestOptional: false },
});

function fail(code) {
  throw new Error(code);
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

/**
 * Flattens what `gh api --paginate --slurp` printed: an array of pages, each an array of objects.
 * Inputs: the pages and a label for the error. Output: the objects in page order. Any other shape
 * is refused, so page one alone, or pages not wrapped by --slurp, never pass for the whole list.
 */
function flatPages(pages, label) {
  const object = (item) => item !== null && typeof item === "object" && !Array.isArray(item);
  if (!Array.isArray(pages) || pages.length === 0 || !pages.every((page) => Array.isArray(page) && page.every(object))) fail(`dev_build_pages_rejected:${label}`);
  return pages.flat();
}

/**
 * Orders two build tags newest first: by the 12-digit UTC stamp in the name, then by the X.Y.Z
 * base as numbers. Inputs: two names that match BUILD_TAG. Output: a sort comparator value.
 */
function newerFirst(left, right) {
  const a = BUILD_TAG.exec(left);
  const b = BUILD_TAG.exec(right);
  assert(a && b, "dev_build_order_invariant");
  if (a[4] !== b[4]) return a[4] < b[4] ? 1 : -1;
  for (const part of [1, 2, 3]) if (Number(a[part]) !== Number(b[part])) return Number(b[part]) - Number(a[part]);
  return 0;
}

/**
 * The dev builds that count: published prereleases whose tag matches BUILD_TAG, newest first.
 * Input: the flat release list. Output: those releases; two of one tag are refused.
 */
function counted(releases) {
  const builds = releases.filter((release) => typeof release.tag_name === "string" && BUILD_TAG.test(release.tag_name) && release.draft === false && release.prerelease === true);
  if (new Set(builds.map((release) => release.tag_name)).size !== builds.length) fail("dev_build_ambiguous");
  return builds.sort((a, b) => newerFirst(a.tag_name, b.tag_name));
}

/**
 * Finds the release of one dev build tag. Inputs: the slurped release pages, the tag and this
 * run's commit. Output: { state: "none" }, or { state: "draft" | "published", id }. Two releases
 * of the tag, another commit's build or a build that is not a prerelease are refused.
 */
export function findBuild({ releases, tag, commit }) {
  if (typeof tag !== "string" || !BUILD_TAG.test(tag)) fail(`dev_build_tag_rejected:${tag}`);
  if (typeof commit !== "string" || !COMMIT.test(commit)) fail("dev_build_commit_rejected");
  const found = flatPages(releases, "releases").filter((release) => release.tag_name === tag);
  if (found.length === 0) return { state: "none" };
  if (found.length > 1) fail(`dev_build_ambiguous:${tag}`);
  const [release] = found;
  if (release.target_commitish !== commit) fail(`dev_build_commit_rejected:${tag}`);
  if (release.prerelease !== true || typeof release.draft !== "boolean") fail(`dev_build_state_rejected:${tag}`);
  if (!Number.isSafeInteger(release.id) || release.id <= 0) fail(`dev_build_id_rejected:${tag}`);
  return { state: release.draft ? "draft" : "published", id: release.id };
}

/**
 * Checks that one dev build release holds exactly `files`. Inputs: the slurped release pages, the
 * tag, this run's commit, name -> { sha256, size } of dist, and the mode: "draft" (this step's
 * draft; an asset the API gives no digest yet passes on its size), "published" (every digest), or
 * "reused" (an earlier attempt's build: its own manifests may differ, every other file may not).
 * Output: the release id.
 */
export function checkBuild({ releases, tag, commit, files, mode }) {
  const rule = Object.hasOwn(MODES, mode) ? MODES[mode] : fail(`dev_build_mode_rejected:${mode}`);
  const found = findBuild({ releases, tag, commit });
  if (found.state === "none") fail(`dev_build_missing:${tag}`);
  if ((found.state === "draft") !== rule.draft) fail(`dev_build_state_rejected:${tag}`);
  const release = flatPages(releases, "releases").find((item) => item.id === found.id);
  const assets = Array.isArray(release.assets) ? release.assets : fail(`dev_build_assets_rejected:${tag}`);
  const names = assets.map((asset) => asset.name).sort();
  const expected = Object.keys(files).sort();
  if (new Set(names).size !== names.length || names.join("\n") !== expected.join("\n")) fail(`dev_build_assets_rejected:${tag}`);
  for (const asset of assets) {
    const file = files[asset.name];
    assert(file && /^[0-9a-f]{64}$/.test(file.sha256) && Number.isSafeInteger(file.size), "dev_build_file_invariant");
    if (asset.state !== "uploaded") fail(`dev_build_asset_rejected:${asset.name}`);
    if (!rule.ownManifests && MANIFESTS.includes(asset.name)) continue;
    if (asset.size !== file.size) fail(`dev_build_asset_rejected:${asset.name}`);
    if (asset.digest === null || asset.digest === undefined) {
      if (rule.digestOptional) continue;
      fail(`dev_build_digest_missing:${asset.name}`);
    }
    if (asset.digest !== `sha256:${file.sha256}`) fail(`dev_build_asset_rejected:${asset.name}`);
  }
  return found.id;
}

/**
 * Decides whether dev may point at one dev build. Inputs: the slurped release pages, the build
 * tag, the repository and the build's three manifest files as downloaded (name -> bytes).
 * Output: { move: true }, or { move: false, newest } when a newer published build exists. A build
 * that is not published, files that are not the build's own bytes, and manifests that announce
 * another version or name any URL other than one of the build's own files are refused.
 */
export function pointerDecision({ releases, tag, repo, files }) {
  if (typeof tag !== "string" || !BUILD_TAG.test(tag)) fail(`dev_build_tag_rejected:${tag}`);
  if (typeof repo !== "string" || !REPO.test(repo)) fail("dev_build_repo_rejected");
  const builds = counted(flatPages(releases, "releases"));
  const ours = builds.find((release) => release.tag_name === tag);
  if (!ours) fail(`dev_build_not_published:${tag}`);
  if (builds[0].tag_name !== tag) return { move: false, newest: builds[0].tag_name };
  const assets = new Map((Array.isArray(ours.assets) ? ours.assets : []).map((asset) => [asset.name, asset]));
  for (const name of MANIFESTS) {
    const asset = assets.get(name);
    const bytes = files[name];
    if (!asset || bytes === undefined || typeof asset.digest !== "string" || asset.digest !== `sha256:${sha256(bytes)}`) fail(`dev_build_pointer_file_rejected:${name}`);
  }
  const base = `https://github.com/${repo}/releases/download/${tag}/`;
  const version = tag.slice("build-".length);
  for (const name of ["latest.json", "cli-latest.json"]) {
    let manifest;
    try {
      manifest = JSON.parse(Buffer.from(files[name]).toString("utf8"));
    } catch {
      fail(`dev_build_pointer_file_rejected:${name}`);
    }
    if (manifest?.version !== version) fail(`dev_build_pointer_version_rejected:${name}`);
    const platforms = manifest.platforms !== null && typeof manifest.platforms === "object" ? Object.values(manifest.platforms) : [];
    if (platforms.length === 0) fail(`dev_build_pointer_url_rejected:${name}`);
    for (const platform of platforms) {
      const url = platform?.url;
      if (typeof url !== "string" || !url.startsWith(base)) fail(`dev_build_pointer_url_rejected:${url}`);
      let file = "";
      try {
        file = decodeURIComponent(url.slice(base.length));
      } catch {
        file = "";
      }
      if (file.includes("/") || MANIFESTS.includes(file) || !assets.has(file)) fail(`dev_build_pointer_url_rejected:${url}`);
    }
  }
  return { move: true };
}

/**
 * Selects the dev builds past the newest `keep` to delete: published prerelease builds, and
 * build tags no listed release holds (left by a release delete whose tag delete failed).
 * Inputs: the slurped release and tag-ref pages, `keep` (10..50) and the build dev now names.
 * Output: `release <tag>` and `tag <tag>` lines, oldest first. A pointer outside the kept builds
 * is refused, so nothing is deleted.
 */
export function pruneSelection({ releases, tags, keep, pointer }) {
  if (!Number.isInteger(keep) || keep < KEEP_MIN || keep > KEEP_MAX) fail("dev_build_keep_rejected");
  const listed = flatPages(releases, "releases");
  const refs = flatPages(tags, "tags");
  const named = new Set(listed.map((release) => release.tag_name));
  const orphans = new Set(refs.map((ref) => (typeof ref.ref === "string" && ref.ref.startsWith("refs/tags/") ? ref.ref.slice("refs/tags/".length) : "")).filter((name) => BUILD_TAG.test(name) && !named.has(name)));
  const entries = [...counted(listed).map((release) => ({ kind: "release", name: release.tag_name })), ...[...orphans].map((name) => ({ kind: "tag", name }))];
  entries.sort((a, b) => newerFirst(a.name, b.name));
  if (!entries.slice(0, keep).some((entry) => entry.kind === "release" && entry.name === pointer)) fail("dev_build_pointer_not_kept");
  const selected = entries.slice(keep).reverse();
  assert(selected.length === Math.max(0, entries.length - keep), "dev_build_selection_count_invariant");
  for (const entry of selected) {
    assert(BUILD_TAG.test(entry.name) && entry.name !== pointer && !entry.name.startsWith("v") && !entry.name.startsWith("dev"), "dev_build_selection_invariant");
  }
  return selected.map((entry) => `${entry.kind} ${entry.name}`);
}

/**
 * Describes every file in a dist folder for `checkBuild`. Input: the folder. Output:
 * name -> { sha256, size }; anything that is not a regular file is refused.
 */
function described(folder) {
  const files = {};
  for (const name of readdirSync(folder)) {
    const path = join(folder, name);
    if (!lstatSync(path).isFile()) fail(`dev_build_dist_rejected:${name}`);
    const bytes = readFileSync(path);
    files[name] = { sha256: sha256(bytes), size: bytes.length };
  }
  return files;
}

/**
 * The release workflow's entry: `find <releases.json> <tag> <commit>`, `check <releases.json>
 * <tag> <commit> <dist> <mode>`, `pointer <releases.json> <tag> <repo> <folder>` and `select
 * <releases.json> <tags.json> <keep> <pointer>`. Input: the arguments. Output: the text to print.
 */
function main([command, ...rest]) {
  const json = (path) => JSON.parse(readFileSync(path, "utf8"));
  if (command === "find" && rest.length === 3) {
    const found = findBuild({ releases: json(rest[0]), tag: rest[1], commit: rest[2] });
    return found.state === "none" ? "none" : `${found.state} ${found.id}`;
  }
  if (command === "check" && rest.length === 5) {
    return String(checkBuild({ releases: json(rest[0]), tag: rest[1], commit: rest[2], files: described(rest[3]), mode: rest[4] }));
  }
  if (command === "pointer" && rest.length === 4) {
    const files = Object.fromEntries(MANIFESTS.map((name) => [name, readFileSync(join(rest[3], name))]));
    const decision = pointerDecision({ releases: json(rest[0]), tag: rest[1], repo: rest[2], files });
    return decision.move ? "move" : `keep ${decision.newest}`;
  }
  if (command === "select" && rest.length === 4) {
    if (!/^[1-9]\d{0,2}$/.test(rest[2])) fail("dev_build_keep_rejected");
    return pruneSelection({ releases: json(rest[0]), tags: json(rest[1]), keep: Number(rest[2]), pointer: rest[3] }).join("\n");
  }
  fail("dev_build_arguments_rejected");
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  try {
    const out = main(process.argv.slice(2));
    if (out !== "") console.log(out);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
