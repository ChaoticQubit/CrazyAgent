import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { checkBuild, findBuild, pointerDecision, pruneSelection } from "./dev-builds.mjs";

const COMMIT = "0123456789abcdef0123456789abcdef01234567";
const OTHER_COMMIT = "fedcba9876543210fedcba9876543210fedcba98";
const REPO = "example/fake-repo";
let nextId = 100;

/** The sha256 of a text as hex. Input: the text. Output: the hex digest. */
function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

/** A dev build tag. Inputs: the X.Y.Z base and the 12-digit stamp. Output: build-<base>-dev.<stamp>. */
function tag(base, stamp) {
  return `build-${base}-dev.${stamp}`;
}

/**
 * A release as the releases API lists it, published as a prerelease from COMMIT unless `extra`
 * says otherwise. Inputs: the tag name, its files (name -> text) and overrides. Output: the release.
 */
function release(tagName, files = {}, extra = {}) {
  nextId += 1;
  const assets = Object.entries(files).map(([name, text]) => ({ name, size: Buffer.byteLength(text), state: "uploaded", digest: `sha256:${sha256(text)}` }));
  return { id: nextId, tag_name: tagName, draft: false, prerelease: true, target_commitish: COMMIT, assets, ...extra };
}

/** `count` published builds of base 2.4.0, stamped one minute apart from 202611131000, oldest first. */
function builds(count) {
  return Array.from({ length: count }, (_, index) => release(tag("2.4.0", String(202611131000 + index))));
}

/** A ref as the matching-refs API lists it. Input: the tag name. Output: { ref }. */
function ref(name) {
  return { ref: `refs/tags/${name}` };
}

/** The files a dev build's checks read: name -> { sha256, size }. Input: name -> text. */
function described(files) {
  return Object.fromEntries(Object.entries(files).map(([name, text]) => [name, { sha256: sha256(text), size: Buffer.byteLength(text) }]));
}

const BUILD = tag("2.4.0", "202611131207");
const BASE = `https://github.com/${REPO}/releases/download/${BUILD}`;

/**
 * The files of one dev build whose manifests name `base`. Inputs: the base URL, the version the
 * manifests announce and their pub_date. Output: name -> text, manifests and SHA256SUMS included.
 */
function buildFiles(base = BASE, version = "2.4.0-dev.202611131207", pubDate = "2026-11-13T12:07:00Z") {
  const files = { "app.tar.gz": "desktop bytes", "app.tar.gz.sig": "desktop signature", "cli.tar.gz": "cli bytes", "cli.tar.gz.sig": "cli signature" };
  files["latest.json"] = JSON.stringify({ version, notes: "Dev build", pub_date: pubDate, platforms: { "darwin-aarch64": { signature: files["app.tar.gz.sig"], url: `${base}/app.tar.gz` } } });
  files["cli-latest.json"] = JSON.stringify({ version, channel: "dev", commit: COMMIT, pub_date: pubDate, platforms: { "aarch64-apple-darwin": { url: `${base}/cli.tar.gz`, sha256: sha256(files["cli.tar.gz"]), signature: files["cli.tar.gz.sig"] } } });
  files.SHA256SUMS = Object.keys(files).sort().map((name) => `${sha256(files[name])}  ${name}\n`).join("");
  return files;
}

/** The three files the dev pointer copies from a build. Input: the build's files. Output: name -> text. */
function pointerFiles(files) {
  return { "latest.json": files["latest.json"], "cli-latest.json": files["cli-latest.json"], SHA256SUMS: files.SHA256SUMS };
}

test("prune keeps the newest 15 of 20 dev builds and deletes the 5 oldest, oldest first", () => {
  const all = builds(20);
  const lines = pruneSelection({ releases: [all], tags: [all.map((item) => ref(item.tag_name))], keep: 15, pointer: all.at(-1).tag_name });
  assert.deepEqual(lines, all.slice(0, 5).map((item) => `release ${item.tag_name}`));
});

test("prune orders builds by their 12-digit stamp, then by the X.Y.Z base as numbers", () => {
  const fillers = Array.from({ length: 9 }, (_, index) => release(tag("2.4.1", String(202611140000 + index))));
  const pointer = fillers.at(-1).tag_name;
  for (const [older, newer] of [
    [tag("2.4.0", "202611131207"), tag("2.4.1", "202611131300")],
    [tag("2.4.1", "202611131207"), tag("2.4.0", "202611131300")],
    [tag("2.4.0", "202611131300"), tag("2.4.1", "202611131300")],
    [tag("2.9.0", "202611131300"), tag("2.10.0", "202611131300")],
  ]) {
    const lines = pruneSelection({ releases: [[release(newer), ...fillers, release(older)]], tags: [[]], keep: 10, pointer });
    assert.deepEqual(lines, [`release ${older}`], `${older} is older than ${newer}`);
  }
});

test("prune never selects dev, a v* release, a draft or a non-prerelease build, or a malformed build name", () => {
  const all = builds(20);
  const old = "202601010000";
  const others = [
    release("dev"), release("v2.3.2", {}, { prerelease: false }), release("v2.2.0", {}, { prerelease: false }),
    release(tag("2.0.0", old), {}, { draft: true }), release(tag("2.0.1", old), {}, { prerelease: false }),
    ...["build-2.4.0", "build-2.4.0-dev.1234", "build-2.4.0-dev.202601010000x", "Build-2.4.0-dev.202601010000", "build-02.4.0-dev.202601010000", "v2.4.0-dev.202601010000"].map((name) => release(name)),
  ];
  const refs = [...all, ...others].map((item) => ref(item.tag_name));
  const lines = pruneSelection({ releases: [[...others.slice(0, 5), ...all, ...others.slice(5)]], tags: [[...refs, ref("build-2.4.0-dev.12345678901")]], keep: 10, pointer: all.at(-1).tag_name });
  assert.deepEqual(lines, all.slice(0, 10).map((item) => `release ${item.tag_name}`));
});

test("prune deletes an old orphan build tag, keeps a recent one, and never takes the tag of a listed release for an orphan", () => {
  const all = builds(12);
  const draft = release(tag("2.4.0", "202611120000"), {}, { draft: true });
  const plain = release(tag("2.4.0", "202611120001"), {}, { prerelease: false });
  const oldOrphan = tag("2.3.9", "202611110000");
  const newOrphan = tag("2.4.0", "202611139999");
  const lines = pruneSelection({
    releases: [[...all, draft, plain]],
    tags: [[...all.map((item) => ref(item.tag_name)), ref(draft.tag_name), ref(plain.tag_name), ref(oldOrphan), ref(newOrphan)]],
    keep: 10,
    pointer: all.at(-1).tag_name,
  });
  assert.deepEqual(lines, [`tag ${oldOrphan}`, ...all.slice(0, 3).map((item) => `release ${item.tag_name}`)]);
});

test("prune refuses, deleting nothing, a keep outside 10..50 and a pointer that is not a kept, published build", () => {
  const all = builds(20);
  const tags = [all.map((item) => ref(item.tag_name))];
  for (const keep of [9, 51, 15.5, "15", Number.NaN]) {
    assert.throws(() => pruneSelection({ releases: [all], tags, keep, pointer: all.at(-1).tag_name }), /^Error: dev_build_keep_rejected$/, String(keep));
  }
  const draft = release(tag("2.4.0", "202611139999"), {}, { draft: true });
  for (const pointer of [all[0].tag_name, draft.tag_name, tag("2.4.0", "202611139998"), "dev", "v2.3.2"]) {
    assert.throws(() => pruneSelection({ releases: [[...all, draft]], tags, keep: 15, pointer }), /^Error: dev_build_pointer_not_kept$/, pointer);
  }
});

test("prune selects nothing while there are no more builds than it keeps", () => {
  const all = builds(8);
  assert.deepEqual(pruneSelection({ releases: [all], tags: [all.map((item) => ref(item.tag_name))], keep: 15, pointer: all.at(-1).tag_name }), []);
});

test("prune reads every page: a release on page two keeps its tag from being taken for an orphan, and a list that is not an array of pages is refused", () => {
  const all = builds(13);
  const late = all[0];
  const lines = pruneSelection({ releases: [all.slice(1), [late]], tags: [all.slice(1).map((item) => ref(item.tag_name)), [ref(late.tag_name)]], keep: 10, pointer: all.at(-1).tag_name });
  assert.deepEqual(lines, all.slice(0, 3).map((item) => `release ${item.tag_name}`), "the page-two release is selected as a release, never as an orphan tag");
  assert(!lines.includes(`tag ${late.tag_name}`));
  for (const [releases, tags] of [[all, [[]]], [[all], []], [[all], [ref("x")]], [[all, null], [[]]], [{}, [[]]], [[[1]], [[]]]]) {
    assert.throws(() => pruneSelection({ releases, tags, keep: 10, pointer: all.at(-1).tag_name }), /^Error: dev_build_pages_rejected:(releases|tags)$/);
  }
});

test("finding a build tells none, this run's draft and a published build apart, and refuses two releases of one tag or another commit's build", () => {
  const draft = release(BUILD, {}, { draft: true });
  const published = release(BUILD);
  assert.deepEqual(findBuild({ releases: [[]], tag: BUILD, commit: COMMIT }), { state: "none" });
  assert.deepEqual(findBuild({ releases: [[release("dev")], [draft]], tag: BUILD, commit: COMMIT }), { state: "draft", id: draft.id });
  assert.deepEqual(findBuild({ releases: [[published]], tag: BUILD, commit: COMMIT }), { state: "published", id: published.id });
  assert.throws(() => findBuild({ releases: [[draft], [release(BUILD, {}, { draft: true })]], tag: BUILD, commit: COMMIT }), new RegExp(`^Error: dev_build_ambiguous:${BUILD}$`));
  assert.throws(() => findBuild({ releases: [[release(BUILD, {}, { target_commitish: OTHER_COMMIT })]], tag: BUILD, commit: COMMIT }), new RegExp(`^Error: dev_build_commit_rejected:${BUILD}$`));
  assert.throws(() => findBuild({ releases: [[release(BUILD, {}, { prerelease: false })]], tag: BUILD, commit: COMMIT }), new RegExp(`^Error: dev_build_state_rejected:${BUILD}$`));
  assert.throws(() => findBuild({ releases: [[]], tag: "v2.4.0", commit: COMMIT }), /^Error: dev_build_tag_rejected:v2\.4\.0$/);
});

test("checking a build binds it to this commit and to every file's sha256, not to names and sizes", () => {
  const files = buildFiles();
  const dist = described(files);
  const draft = release(BUILD, files, { draft: true });
  assert.equal(checkBuild({ releases: [[draft]], tag: BUILD, commit: COMMIT, files: dist, mode: "draft" }), draft.id);
  const published = release(BUILD, files);
  assert.equal(checkBuild({ releases: [[published]], tag: BUILD, commit: COMMIT, files: dist, mode: "published" }), published.id);
  const sameSize = release(BUILD, { ...files, "app.tar.gz": "desktop bytez" });
  for (const mode of ["published", "reused"]) {
    assert.throws(() => checkBuild({ releases: [[sameSize]], tag: BUILD, commit: COMMIT, files: dist, mode }), /^Error: dev_build_asset_rejected:app\.tar\.gz$/, mode);
  }
  const missing = release(BUILD, Object.fromEntries(Object.entries(files).filter(([name]) => name !== "cli.tar.gz.sig")));
  assert.throws(() => checkBuild({ releases: [[missing]], tag: BUILD, commit: COMMIT, files: dist, mode: "published" }), new RegExp(`^Error: dev_build_assets_rejected:${BUILD}$`));
  const starting = release(BUILD, files);
  starting.assets[0].state = "starter";
  assert.throws(() => checkBuild({ releases: [[starting]], tag: BUILD, commit: COMMIT, files: dist, mode: "published" }), /^Error: dev_build_asset_rejected:/);
  assert.throws(() => checkBuild({ releases: [[release(BUILD, files, { target_commitish: OTHER_COMMIT })]], tag: BUILD, commit: COMMIT, files: dist, mode: "reused" }), new RegExp(`^Error: dev_build_commit_rejected:${BUILD}$`));
  assert.throws(() => checkBuild({ releases: [[draft]], tag: BUILD, commit: COMMIT, files: dist, mode: "published" }), new RegExp(`^Error: dev_build_state_rejected:${BUILD}$`));
  assert.throws(() => checkBuild({ releases: [[]], tag: BUILD, commit: COMMIT, files: dist, mode: "published" }), new RegExp(`^Error: dev_build_missing:${BUILD}$`));
  assert.throws(() => checkBuild({ releases: [[published]], tag: BUILD, commit: COMMIT, files: dist, mode: "upload" }), /^Error: dev_build_mode_rejected:upload$/);
});

test("a reused build's own manifests may differ from this attempt's, its other files may not, and only a fresh draft may lack a digest", () => {
  const files = buildFiles();
  const again = described(buildFiles(BASE, "2.4.0-dev.202611131207", "2026-11-13T12:31:00Z"));
  const published = release(BUILD, files);
  assert.equal(checkBuild({ releases: [[published]], tag: BUILD, commit: COMMIT, files: again, mode: "reused" }), published.id);
  assert.throws(() => checkBuild({ releases: [[published]], tag: BUILD, commit: COMMIT, files: again, mode: "published" }), /^Error: dev_build_asset_rejected:(latest\.json|cli-latest\.json|SHA256SUMS)$/);
  const digestless = (extra) => {
    const item = release(BUILD, files, extra);
    for (const asset of item.assets) asset.digest = null;
    return item;
  };
  const draft = digestless({ draft: true });
  assert.equal(checkBuild({ releases: [[draft]], tag: BUILD, commit: COMMIT, files: described(files), mode: "draft" }), draft.id);
  for (const mode of ["published", "reused"]) {
    assert.throws(() => checkBuild({ releases: [[digestless()]], tag: BUILD, commit: COMMIT, files: described(files), mode }), /^Error: dev_build_digest_missing:/, mode);
  }
  const wrongSize = release(BUILD, files, { draft: true });
  wrongSize.assets.find((asset) => asset.name === "cli.tar.gz").digest = null;
  wrongSize.assets.find((asset) => asset.name === "cli.tar.gz").size += 1;
  assert.throws(() => checkBuild({ releases: [[wrongSize]], tag: BUILD, commit: COMMIT, files: described(files), mode: "draft" }), /^Error: dev_build_asset_rejected:cli\.tar\.gz$/);
});

test("dev moves to a build only when it is the newest published one, its three files are the build's own bytes and every manifest URL names one of its files", () => {
  const files = buildFiles();
  const ours = release(BUILD, files);
  const older = release(tag("2.4.0", "202611131100"));
  assert.deepEqual(pointerDecision({ releases: [[older], [ours]], tag: BUILD, repo: REPO, files: pointerFiles(files) }), { move: true });
  const newer = release(tag("2.4.0", "202611131300"));
  assert.deepEqual(pointerDecision({ releases: [[ours, newer]], tag: BUILD, repo: REPO, files: pointerFiles(files) }), { move: false, newest: newer.tag_name });
  assert.deepEqual(pointerDecision({ releases: [[ours, release(tag("2.4.0", "202611131300"), {}, { draft: true }), release(tag("2.4.0", "202611131301"), {}, { prerelease: false })]], tag: BUILD, repo: REPO, files: pointerFiles(files) }), { move: true }, "drafts and non-prereleases are no newer build");
  for (const listed of [[older], [older, release(BUILD, files, { draft: true })]]) {
    assert.throws(() => pointerDecision({ releases: [listed], tag: BUILD, repo: REPO, files: pointerFiles(files) }), new RegExp(`^Error: dev_build_not_published:${BUILD}$`));
  }
  const rebuilt = pointerFiles(buildFiles(BASE, "2.4.0-dev.202611131207", "2026-11-13T12:31:00Z"));
  assert.throws(() => pointerDecision({ releases: [[ours]], tag: BUILD, repo: REPO, files: rebuilt }), /^Error: dev_build_pointer_file_rejected:latest\.json$/, "dev gets the build's own bytes, never a regenerated copy");
});

test("dev never moves to manifests that name the dev release, another build, a file the build lacks or another version", () => {
  for (const [label, files, error] of [
    ["the dev release", buildFiles(`https://github.com/${REPO}/releases/download/dev`), /^Error: dev_build_pointer_url_rejected:https:\/\/github\.com\/example\/fake-repo\/releases\/download\/dev\//],
    ["another build", buildFiles(`https://github.com/${REPO}/releases/download/${tag("2.4.0", "202611131100")}`), /^Error: dev_build_pointer_url_rejected:/],
    ["another repository", buildFiles(`https://github.com/example/other-repo/releases/download/${BUILD}`), /^Error: dev_build_pointer_url_rejected:/],
    ["a v* release", buildFiles(`https://github.com/${REPO}/releases/download/v2.4.0`), /^Error: dev_build_pointer_url_rejected:/],
    ["another version", buildFiles(BASE, "2.4.0-dev.202611131100"), /^Error: dev_build_pointer_version_rejected:latest\.json$/],
  ]) {
    assert.throws(() => pointerDecision({ releases: [[release(BUILD, files)]], tag: BUILD, repo: REPO, files: pointerFiles(files) }), error, label);
  }
  const files = buildFiles();
  const latest = JSON.parse(files["latest.json"]);
  for (const url of [`${BASE}/missing.tar.gz`, `${BASE}/SHA256SUMS`, `${BASE}/sub/app.tar.gz`, `${BASE}/`]) {
    latest.platforms["darwin-aarch64"].url = url;
    const changed = { ...files, "latest.json": JSON.stringify(latest) };
    assert.throws(() => pointerDecision({ releases: [[release(BUILD, changed)]], tag: BUILD, repo: REPO, files: pointerFiles(changed) }), /^Error: dev_build_pointer_url_rejected:/, url);
  }
  latest.platforms = {};
  const empty = { ...files, "latest.json": JSON.stringify(latest) };
  assert.throws(() => pointerDecision({ releases: [[release(BUILD, empty)]], tag: BUILD, repo: REPO, files: pointerFiles(empty) }), /^Error: dev_build_pointer_url_rejected:latest\.json$/);
});
