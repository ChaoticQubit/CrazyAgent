import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "node:http";
import { DatabaseSync } from "node:sqlite";
import { crc32, deflateSync } from "node:zlib";
import { assertMasked, main, mask, printPdf, readLivePass, readRetro, readTrial, renderReport } from "./report.mjs";

const REPORT = join(dirname(fileURLToPath(import.meta.url)), "report.mjs");
const GIF = Buffer.from("47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b", "hex");

const SECRETS = {
  anthropic: "sk-ant-api03-" + "Ab1".repeat(14),
  openai: "sk-proj-" + "Z9x".repeat(14),
  bearer: "abcDEF0123456789ghiJKLmno",
  jwt: "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJvd25lciJ9.c2lnbmF0dXJlLXBhcnQ",
  oauth: "ya29.a0AfH6SMBx" + "q".repeat(20),
  refresh: "1//0gRefreshTokenValue" + "r".repeat(12),
  sessionKey: "sk-ant-sid01-" + "s".repeat(30),
  sessionToken: "st_" + "9".repeat(24),
  deviceCode: "QXRT-PLMN",
  deviceJson: "device-code-value-" + "d".repeat(12),
  cookie: "cookievalue" + "c".repeat(16),
  email: "owner.person@example.com",
  account: "6fe96b44fe9c4363ac33",
  chatgpt: "04b86c5c30d2a2bb16e4",
  uuid: "3f6c2b1e-1111-4222-8333-944455556666",
  github: "ghp_" + "G".repeat(36),
  envKey: "plainkeyvalue" + "k".repeat(10),
  oauthCode: "authcode" + "x".repeat(16),
  privateKey: "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcw",
  urlPassword: "hunter2pass",
  authBasic: "dXNlcjpwYXNzd29yZA==",
};

const LEAKY = [
  `ANTHROPIC_API_KEY=${SECRETS.anthropic}`,
  `export OPENAI_KEY_VALUE ${SECRETS.openai}`,
  `Authorization: Bearer ${SECRETS.bearer}`,
  `the id token ${SECRETS.jwt} came back`,
  `{"access_token": "${SECRETS.oauth}", "refresh_token": "${SECRETS.refresh}"}`,
  `cookie jar sessionKey=${SECRETS.sessionKey}`,
  `{"session_token": "${SECRETS.sessionToken}"}`,
  `Open the page and enter the code ${SECRETS.deviceCode} to sign in`,
  `{"device_code": "${SECRETS.deviceJson}"}`,
  `Cookie: __Secure-next-auth.session-token=${SECRETS.cookie}`,
  `signed in as ${SECRETS.email}`,
  `('claude-account-${SECRETS.account}', 'max', 3.0, 100.0)`,
  `('chatgpt-account-${SECRETS.chatgpt}', None)`,
  `{"account_uuid": "${SECRETS.uuid}"}`,
  `remote token ${SECRETS.github}`,
  `OPENAI_API_KEY=${SECRETS.envKey}`,
  `redirect to http://localhost:1455/auth/callback?code=${SECRETS.oauthCode}&scope=openid`,
  `-----BEGIN PRIVATE KEY-----\n${SECRETS.privateKey}\n-----END PRIVATE KEY-----`,
  `git clone https://botuser:${SECRETS.urlPassword}@git.example.org/repo.git`,
  `Proxy-Authorization: Basic ${SECRETS.authBasic}`,
].join("\n");

const HEADERS = {
  basic: "Ym90dXNlcjpodW50ZXIycGFzcw==",
  hex: "4f9a1c2e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39",
  shortBearer: "plainbearervalue",
  sid: "q7Zq7Zq7Zq7Zq7Zq7Zq7Z",
  org: "7d3e1c2a-5555-4666-8777-988899990000",
  cfbm: "cfbmvalue" + "b".repeat(12),
  puid: "user-" + "P".repeat(20),
  jwe: "eyJhbGciOiJkaXIiLCJlbmMiOiJBMjU2R0NNIn0..aXZpdml2aXZpdg.Y2lwaGVydGV4dGNpcGhlcnRleHQ.dGFndGFndGFndGFn",
};

const HEADER_LINES = [
  `curl -sS -H 'Authorization: Basic ${HEADERS.basic}' https://api.example.org/v1`,
  `sent with Authorization: token ${HEADERS.hex} to the API`,
  `retrying with Authorization: Bearer ${HEADERS.shortBearer} after 401`,
  `request headers Cookie: theme=dark; sid=${HEADERS.sid}; lastActiveOrg=${HEADERS.org}`,
  `< set-cookie: __cf_bm=${HEADERS.cfbm}; _puid=${HEADERS.puid}; Path=/; HttpOnly`,
  `the session ${HEADERS.jwe} expired`,
].join("\n");

const ENCODED = {
  base64: Buffer.from('{"access_token":"enc0dedAccessT0kenValue1"}').toString("base64"),
  urlToken: "urlEnc0dedTokenValue22",
  escaped: "esc4pedRefreshValue333",
  unicodeTail: "Un1c0deTail" + "u".repeat(20),
  entityTail: "Ent1tyTail" + "e".repeat(20),
  tspanTail: "Tsp4nTail" + "t".repeat(20),
};

const ENCODED_LINES = [
  `payload ${ENCODED.base64}`,
  `redirect next=%2Fcallback%3Faccess_token%3D${ENCODED.urlToken}%26x%3D1`,
  `{\\"refresh_token\\":\\"${ENCODED.escaped}\\"}`,
  `{"note": "sk\\u002dant\\u002dapi03\\u002d${ENCODED.unicodeTail}"}`,
].join("\n");

const WRAPPED_FRAME = [
  "  ┌─ Login ────────────────────┐",
  "  │ id eyJhbGciOiJSUzI1NiJ9.ey │",
  "  │ JzdWIiOiJ3cmFwIn0.d3JhcHBl │",
  "  │ ZC1zaWduYXR1cmU            │",
  "  │ key sk-ant-api03-Wr4pWr4pW │",
  "  │ r4pWr4pWr4pWr4pWr4pWr4pWr4 │",
  "  └────────────────────────────┘",
].join("\n");

const WRAPPED_PARTS = ["JzdWIiOiJ3cmFwIn0", "d3JhcHBl", "ZC1zaWduYXR1cmU", "r4pWr4pWr4pWr4pWr4pWr4pWr4"];

function svgText(body) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="240" height="40"><text x="4" y="20">${body}</text></svg>`;
}

function section(html, id) {
  const from = html.indexOf(`<article class="check" id="${id}">`);
  return from < 0 ? "" : html.slice(from, html.indexOf("</article>", from));
}

async function engineRoot(names) {
  const root = await folder("trial-report-engine-");
  const db = new DatabaseSync(join(root, "engine.db"));
  try {
    db.exec("CREATE TABLE accounts (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, body BLOB NOT NULL)");
    const insert = db.prepare("INSERT INTO accounts (id, body) VALUES (?, ?)");
    for (const [index, name] of names.entries()) {
      insert.run(`claude-account-${String(index).padStart(20, "0")}`, Buffer.from(JSON.stringify({ connection: { harness: "claude", auth: { method: "subscription" } }, details: { name, provider: "anthropic" } })));
    }
  } finally {
    db.close();
  }
  return root;
}

async function addCheck(root, id, evidence) {
  await put(root, `checks/${id}.json`, JSON.stringify({ id, title: id, proves: "review finding", why: "rv-secreview r1", how: "the report", result: "PASS", detail: "", evidence }));
}

function png() {
  const chunk = (type, data) => {
    const body = Buffer.concat([Buffer.from(type, "latin1"), data]);
    const size = Buffer.alloc(4);
    size.writeUInt32BE(data.length);
    const check = Buffer.alloc(4);
    check.writeUInt32BE(crc32(body));
    return Buffer.concat([size, body, check]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1, 0);
  header.writeUInt32BE(1, 4);
  header.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.from([0, 255, 0, 0, 255]))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

async function folder(prefix) {
  const root = await realpath(await mkdtemp(join(tmpdir(), prefix)));
  await chmod(root, 0o700);
  return root;
}

async function put(root, path, content) {
  await mkdir(dirname(join(root, path)), { recursive: true });
  await writeFile(join(root, path), content, { mode: 0o600 });
}

/**
 * Writes a stand-in Chrome that records its arguments, starts one helper in its own process group as
 * Chrome's helpers are, writes both process ids, then waits on the helper and never prints.
 * Input: the folder it goes in. Output: the stand-in path and the argument and process id files.
 */
async function hangingChrome(root) {
  assert.ok(!root.includes("'"), "the folder path is quoted as is");
  const argv = join(root, "argv.txt");
  const pids = join(root, "pids.txt");
  const chrome = join(root, "hanging-chrome");
  await writeFile(chrome, `#!/bin/sh\nprintf '%s\\n' "$@" > '${argv}'\n/bin/sleep 300 &\necho "$$ $!" > '${pids}'\nwait\n`, { mode: 0o700 });
  return { chrome, argv, pids };
}

/**
 * Waits up to five seconds for every listed process to end and be reaped.
 * Input: the process ids. Output: the ids still present at the deadline, empty once all are gone.
 */
async function stillRunning(pids) {
  const deadline = Date.now() + 5_000;
  for (;;) {
    const left = pids.filter((pid) => {
      try {
        process.kill(pid, 0);
        return true;
      } catch (error) {
        return error.code !== "ESRCH";
      }
    });
    if (left.length === 0 || Date.now() > deadline) return left;
    await new Promise((done) => setTimeout(done, 50));
  }
}

/**
 * Reads the stand-in's two process ids once it has written them, for up to twenty seconds.
 * Input: the stand-in's files and a check that fails when its caller ended first. Output: the ids.
 */
async function standInPids(stand, ended = () => false) {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const text = await readFile(stand.pids, "utf8").catch(() => "");
    const pids = text.trim().split(" ").map(Number).filter((pid) => Number.isInteger(pid) && pid > 1);
    if (pids.length === 2) return pids;
    assert.ok(!ended(), "the report command ended before it started Chrome");
    assert.ok(Date.now() < deadline, "the stand-in Chrome never started");
    await new Promise((done) => setTimeout(done, 50));
  }
}

async function trialFolder() {
  const root = await folder("trial-report-");
  await put(root, "manifest.json", JSON.stringify({
    trial: 3, build: "32c3edf6", app_version: "18.13", started: "2026-11-02T23:10:00-05:00", ended: "2026-11-02T23:42:30-05:00",
    project: "/Users/owner/Projects/ca-trial-3", accounts: ["claude-subscription", "codex-chatgpt", "claude-subscription"],
    tokens: { before: 1200, after: 5400 },
  }));
  await put(root, "steps.jsonl", [
    { n: 1, surface: "desktop", title: "Open the project", why: "every check starts from the owner's project page", at: "2026-11-02T23:10:05-05:00", ok: true, detail: "page shown", evidence: ["screenshots/01-open-project.png"] },
    { n: 2, surface: "tui", title: "Answer the For: Me question", why: "the owner answers questions from Questions", at: "2026-11-02T23:20:00-05:00", ok: false, detail: `the sign-in asked for ${SECRETS.deviceCode} from ${SECRETS.email}`, evidence: [] },
    { n: 3, surface: "desktop", title: "Set the project up", why: "the mapping ends on its own", at: "2026-11-02T23:30:00-05:00", ok: false, detail: "BLOCKED: the project's pool was still paused after 901 s (Rate limited until 01:40)", evidence: [] },
  ].map((step) => JSON.stringify(step)).join("\n") + "\n");
  await put(root, "checks/T3-02.json", JSON.stringify({
    id: "T3-02", title: "Lead merges <script>alert(1)</script>", proves: "each lead merges its workers' branches", why: "trial 2 check 7 failed", how: "git log of each lead branch",
    result: "FAIL", detail: `merge refused for claude-account-${SECRETS.account} with Bearer ${SECRETS.bearer}`,
    evidence: [
      { kind: "log", path: "logs/merge.log", caption: "merge log" },
      { kind: "db", path: "db/rows.json", caption: "coord rows" },
      { kind: "frame", path: "frames/f00001.txt", caption: "team tree" },
      { kind: "screenshot", path: "screenshots/02-key.svg", caption: "sign-in page" },
    ],
  }));
  await put(root, "checks/T3-01.json", JSON.stringify({
    id: "T3-01", title: "Tree", proves: "the graph shows every agent", why: "trial 2 check 1", how: "team_agents rows and the TUI frame",
    result: "PASS", detail: "7 rows", evidence: [{ kind: "screenshot", path: "screenshots/01-open-project.png", caption: "project page" }, { kind: "gif", path: "trial.gif", caption: "TUI record" }],
  }));
  await put(root, "checks/T3-03.json", JSON.stringify({ id: "T3-03", title: "Re-ask", proves: "an empty reply is asked again", why: "18.13 fix", how: "turns", result: "UNKNOWN", detail: "not reached", evidence: [] }));
  await put(root, "logs/merge.log", `merge started\n${LEAKY}\nmerge ended\n`);
  await put(root, "db/rows.json", JSON.stringify({ rows: [{ id: 15, state: "read", access_token: SECRETS.oauth, account_uuid: SECRETS.uuid }] }));
  await put(root, "frames/f00001.txt", ` [◆▪◆▪] CrazyAgent  /  ca-trial-3  /  Team\n  ┌─ Team ─┐\n  │ key ${SECRETS.anthropic} │\n  │ ${SECRETS.email} │\n`);
  await put(root, "screenshots/02-key.svg", `<svg xmlns="http://www.w3.org/2000/svg" width="200" height="40"><text x="4" y="20">${SECRETS.jwt} ${SECRETS.email}</text></svg>`);
  await put(root, "screenshots/01-open-project.png", png());
  await put(root, "trial.gif", GIF);
  return root;
}

function svgImages(html) {
  return [...html.matchAll(/data:image\/svg\+xml;base64,([A-Za-z0-9+/=]+)/g)].map((match) => Buffer.from(match[1], "base64").toString("utf8"));
}

test("a trial folder becomes a report with a cover, a summary table and one section per check with its evidence inline", async () => {
  const root = await trialFolder();
  try {
    const html = renderReport(await readTrial(root));
    assert.match(html, /<h1>Trial 3 report<\/h1>/);
    for (const field of ["32c3edf6", "18.13", "2026-11-02", "claude-subscription ×2", "codex-chatgpt ×1", "32 min 30 s", "before 1200", "after 5400", "used 4200"]) {
      assert.ok(html.includes(field), `cover holds ${field}`);
    }
    const summary = html.slice(html.indexOf('<table class="summary">'), html.indexOf("</table>", html.indexOf('<table class="summary">')));
    assert.deepEqual([...summary.matchAll(/<td>(T3-0\d)<\/td>.*?<span class="result (\w+)">/gs)].map((row) => [row[1], row[2]]), [["T3-01", "pass"], ["T3-02", "fail"], ["T3-03", "unknown"]]);
    for (const id of ["T3-01", "T3-02", "T3-03"]) assert.ok(html.includes(`<article class="check" id="${id}">`), `${id} has its own section`);
    for (const label of ["Proves", "Why", "How", "Result", "Detail"]) assert.ok(html.includes(`<dt>${label}</dt>`), `checks show ${label}`);
    assert.ok(html.includes("each lead merges its workers&#39; branches"));
    assert.ok(html.includes("Lead merges &lt;script&gt;alert(1)&lt;/script&gt;"));
    assert.ok(!html.includes("<script"), "agent text never becomes markup");
    assert.ok(html.includes("merge started") && html.includes("merge ended"), "the log excerpt is inline");
    assert.ok(html.includes("&quot;state&quot;: &quot;read&quot;"), "the db rows are inline and pretty printed");
    assert.match(html, /<img alt="project page" src="data:image\/png;base64,/);
    assert.match(html, /<img alt="TUI record" src="data:image\/gif;base64,/);
    assert.ok(svgImages(html).some((svg) => svg.includes("ca-trial-3") && svg.includes("Team")), "a text frame is drawn as an image");
    assert.ok(html.includes("T3-03 has no evidence item."), "a check with no evidence says so");
    assert.ok(html.includes("Answer the For: Me question") && html.includes("the owner answers questions from Questions"), "steps show what and why");
    assert.ok(html.includes("<th>Why</th><th>OK</th>"), "contract steps show why and whether they worked");
    const steps = html.slice(html.indexOf('<table class="steps">'), html.indexOf("</table>", html.indexOf('<table class="steps">')));
    assert.deepEqual([...steps.matchAll(/<span class="result (\w+)">/g)].map((badge) => badge[1]), ["pass", "fail", "blocked"], "a failed step whose detail says BLOCKED is shown blocked");
    const images = html.slice(html.indexOf('<section class="images">'));
    assert.ok(images.includes("screenshots/01-open-project.png") && images.includes("trial.gif"), "every unmasked image is listed");
    assert.ok(!images.includes("02-key.svg") && !images.includes("f00001.txt"), "masked images are not in the unmasked list");
    assert.ok(images.includes("the recorded TUI text held values the report masked"), "the gif is flagged when its frames held secrets");
  } finally {
    await rm(root, { recursive: true });
  }
});

test("every planted secret pattern is masked in the report html, the text frames and the svg images", async () => {
  const root = await trialFolder();
  try {
    const html = renderReport(await readTrial(root));
    const shown = [html, ...svgImages(html)].join("\n");
    for (const [name, value] of Object.entries(SECRETS)) assert.ok(!shown.includes(value), `${name} is masked`);
    assert.ok(shown.includes("[masked]") && shown.includes("[email]") && shown.includes("claude-account-[masked]"));
    assert.ok(html.includes("merge refused for claude-account-[masked] with Bearer [masked]"), "the words around a secret stay");
    assert.equal(assertMasked(html), html);
    assert.throws(() => assertMasked(`<p>${SECRETS.email}</p>`), /report_mask_residue/);
    assert.throws(() => assertMasked(`<p>token=&quot;${SECRETS.sessionToken}&quot;</p>`), /report_mask_residue/);
    assert.equal(mask(mask(LEAKY)), mask(LEAKY), "masking twice changes nothing");
    assert.equal(mask("T3-05 DEC-112 2026-11-02 23:10:05 tokens: 4 exit 101 Bearer values"), "T3-05 DEC-112 2026-11-02 23:10:05 tokens: 4 exit 101 Bearer values");
  } finally {
    await rm(root, { recursive: true });
  }
});

test("evidence outside the trial folder, through a link or with an absolute path is refused and never embedded", async () => {
  const root = await trialFolder();
  const outside = await folder("trial-report-outside-");
  try {
    await put(outside, "secret.log", "OUTSIDE-THE-RUN-FOLDER");
    await symlink(join(outside, "secret.log"), join(root, "logs", "linked.log"));
    await symlink(outside, join(root, "linked-dir"));
    await put(root, "checks/T3-09.json", JSON.stringify({
      id: "T3-09", title: "Escapes", proves: "nothing", why: "test", how: "test", result: "PASS", detail: "",
      evidence: [
        { kind: "log", path: "../" + outside.split("/").pop() + "/secret.log", caption: "parent" },
        { kind: "log", path: join(outside, "secret.log"), caption: "absolute" },
        { kind: "log", path: "logs/linked.log", caption: "file link" },
        { kind: "log", path: "linked-dir/secret.log", caption: "folder link" },
        { kind: "log", path: "logs/absent.log", caption: "absent" },
      ],
    }));
    const html = renderReport(await readTrial(root));
    assert.ok(!html.includes("OUTSIDE-THE-RUN-FOLDER"));
    const section = html.slice(html.indexOf('<article class="check" id="T3-09">'), html.indexOf("</article>", html.indexOf('<article class="check" id="T3-09">')));
    assert.equal([...section.matchAll(/refused: /g)].length, 4, section);
    assert.ok(section.includes("missing: logs/absent.log"));
  } finally {
    await rm(root, { recursive: true });
    await rm(outside, { recursive: true });
  }
});

test("a live pass folder becomes one row per listed test with its group, what it proves, result, duration and a log excerpt only on failure", async () => {
  const logs = await folder("live-report-");
  const repo = await folder("live-report-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    await put(repo, "live-pass.sh", [
      "#!/bin/bash",
      "TESTS=$(",
      "  cat <<'EOF'",
      "local|claude_bare_loads|claude_bare_loads_a_plugin_dir|60|claude",
      "keychain|lib|engine::native::keychain::tests::private_home_gets_its_own_keychain|15|host",
      "signin|lib|engine::signin::tests::a_real_sign_in_finishes|30|host",
      "profile|lib|engine::profile::tests::never_reached|20|profile",
      "EOF",
      ")",
      "",
    ].join("\n"));
    await put(repo, "crates/crazyagent/tests/claude_bare_loads.rs", "/// Claude in bare mode loads the plugin dir's skills and runs none of its hooks.\n#[test]\n#[ignore]\nfn claude_bare_loads_a_plugin_dir() {}\n");
    await put(repo, "crates/crazyagent/src/other/keychain.rs", "/// The wrong module's doc.\nfn private_home_gets_its_own_keychain() {}\n");
    await put(repo, "crates/crazyagent/src/engine/native/keychain.rs", "mod tests {\n    /// A private home gets its own keychain\n    /// that no other home sees.\n    #[tokio::test]\n    async fn private_home_gets_its_own_keychain() {}\n}\n");
    await put(logs, "summary.txt", [
      "== local: no account, no tokens",
      "PASS claude_bare_loads_a_plugin_dir 61s",
      "== keychain: temporary keychains; keep the app idle",
      `FAIL engine::native::keychain::tests::private_home_gets_its_own_keychain 14s exit 101: ${logs}/engine.native.keychain.tests.private_home_gets_its_own_keychain.log`,
      "== signin: DEC-108 real sign-in",
      "BLOCKED engine::signin::tests::a_real_sign_in_finishes: BLOCKED: claude must be on PATH",
      "passed 1, failed or blocked 2",
      "",
    ].join("\n"));
    await put(logs, "claude_bare_loads_a_plugin_dir.log", "running 1 test\ntest claude_bare_loads_a_plugin_dir ... ok\nPASS-LOG-BODY\ntest result: ok. 1 passed\n");
    await put(logs, "engine.native.keychain.tests.private_home_gets_its_own_keychain.log", [
      ...Array.from({ length: 50 }, (_, n) => `setup line ${n}`),
      "thread 'engine::native::keychain::tests::private_home_gets_its_own_keychain' panicked at src/engine/native/keychain.rs:40:5:",
      `assertion failed: the keychain listed ${SECRETS.anthropic} for ${SECRETS.email}`,
      "test result: FAILED. 0 passed; 1 failed",
    ].join("\n"));
    const html = renderReport(await readLivePass(logs, { script, repo }));
    const rows = [...html.matchAll(/<tr class="test"><td>(\d+)<\/td><td>(\w+)<\/td><td><code>([^<]+)<\/code><\/td><td>(.*?)<\/td><td><span class="result [\w-]+">([A-Z ]+)<\/span><\/td><td>([^<]*)<\/td><\/tr>/g)];
    assert.deepEqual(rows.map((row) => [row[2], row[3].split("::").pop(), row[5], row[6]]), [
      ["local", "claude_bare_loads_a_plugin_dir", "PASS", "61 s"],
      ["keychain", "private_home_gets_its_own_keychain", "FAIL", "14 s"],
      ["signin", "a_real_sign_in_finishes", "BLOCKED", ""],
      ["profile", "never_reached", "NOT RUN", ""],
    ]);
    assert.ok(rows[0][4].includes("Claude in bare mode loads the plugin dir&#39;s skills and runs none of its hooks."));
    assert.ok(rows[1][4].includes("A private home gets its own keychain that no other home sees.") && !rows[1][4].includes("wrong module"));
    assert.ok(rows[2][4].includes("A real sign in finishes.") && rows[2][4].includes("from the test name"));
    assert.ok(html.includes("panicked at src/engine/native/keychain.rs:40:5"), "the failure shows its panic");
    assert.ok(!html.includes("setup line 10"), "the excerpt starts near the panic");
    assert.ok(!html.includes("PASS-LOG-BODY"), "a passing test shows no log");
    assert.ok(html.includes("claude must be on PATH"), "a blocked test shows why");
    assert.ok(html.includes("temporary keychains; keep the app idle"), "each group says what it needs");
    for (const value of [SECRETS.anthropic, SECRETS.email]) assert.ok(!html.includes(value));
    assert.ok(html.includes("1 passed, 1 failed, 1 blocked, 1 not run"));
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

function liveScript(rows) {
  return ["#!/bin/bash", "TESTS=$(", "  cat <<'EOF'", ...rows, "EOF", ")", ""].join("\n");
}

function okLog(name) {
  return `running 1 test\ntest ${name} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2990 filtered out; finished in 0.45s\n`;
}

function logName(name) {
  return `${name.replaceAll("::", ".")}.log`;
}

function liveRows(html) {
  return [...html.matchAll(/<tr class="test"><td>\d+<\/td><td>(\w+)<\/td><td><code>([^<]+)<\/code><\/td><td>.*?<\/td><td><span class="result [\w-]+">([A-Z ]+)<\/span>(.*?)<\/td><td>([^<]*)<\/td><\/tr>/g)]
    .map(([, group, name, result, run, seconds]) => [group, name.split("::").pop(), result, run.replace(/<[^>]+>/g, "").trim(), seconds]);
}

test("LP14-1: a test that reports BLOCKED after it ran keeps its reason and its duration", async () => {
  const logs = await folder("live-blocked-");
  const repo = await folder("live-blocked-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    await put(repo, "live-pass.sh", liveScript([
      "local|codex_bridge_live|a_real_codex_app_takes_coord_messages|120|codex-app",
      "profile|lib|team::runtime::live_failover_tests::a_rate_limited_master_fails_over|60|profile",
      "profile|lib|team::runtime::live_network_none_tests::an_api_key_turn_stays_offline|60|codex-key",
    ]));
    await put(logs, "summary.txt", [
      "== local: no account, no tokens",
      "BLOCKED a_real_codex_app_takes_coord_messages 60s: prerequisite: /Applications/Codex.app is not installed",
      "== profile: holds a shared lock on the owner's profile",
      "BLOCKED team::runtime::live_failover_tests::a_rate_limited_master_fails_over 2s: BLOCKED: no account at its limit",
      "BLOCKED team::runtime::live_network_none_tests::an_api_key_turn_stays_offline: BLOCKED: no Codex key profile",
      "passed 0, failed or blocked 3",
      "",
    ].join("\n"));
    const html = renderReport(await readLivePass(logs, { script, repo }));
    assert.deepEqual(liveRows(html), [
      ["local", "a_real_codex_app_takes_coord_messages", "BLOCKED", "", "60 s"],
      ["profile", "a_rate_limited_master_fails_over", "BLOCKED", "", "2 s"],
      ["profile", "an_api_key_turn_stays_offline", "BLOCKED", "", ""],
    ]);
    assert.ok(html.includes("prerequisite: /Applications/Codex.app is not installed"), "a blocked test after its run shows why");
    assert.ok(html.includes("no account at its limit") && !html.includes("BLOCKED: no account"), "the test's own BLOCKED mark is not repeated");
    assert.ok(html.includes("0 passed, 0 failed, 3 blocked, 0 not run"));
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("LP14-2: runs.json merges the listed summaries in order, names the commit of each row's run and lists the runs on the cover", async () => {
  const logs = await folder("live-runs-");
  const repo = await folder("live-runs-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    await put(repo, "live-pass.sh", liveScript([
      "local|lib|launch_context::tests::a_local_turn_answers|30|none",
      "profile|lib|team::runtime::live_claude_tests::under_stop_ends_its_command|60|profile",
      "profile|lib|team::runtime::live_native_lead_tests::a_teammate_cannot_plant|60|profile",
    ]));
    await put(logs, "summary-local.txt", "== local: no account, no tokens\nPASS launch_context::tests::a_local_turn_answers 3s\npassed 1, failed or blocked 0\n");
    await put(logs, "summary-profile.txt", [
      "== profile: holds a shared lock on the owner's profile",
      `FAIL team::runtime::live_claude_tests::under_stop_ends_its_command 8s exit 101: ${logs}/team.runtime.live_claude_tests.under_stop_ends_its_command.log`,
      "BLOCKED team::runtime::live_native_lead_tests::a_teammate_cannot_plant: BLOCKED: not on this checkout",
      "passed 0, failed or blocked 2",
      "",
    ].join("\n"));
    await put(logs, "summary-rerun.txt", "== profile: holds a shared lock on the owner's profile\nPASS team::runtime::live_claude_tests::under_stop_ends_its_command 6s\npassed 1, failed or blocked 0\n");
    await put(logs, "summary.txt", "== profile: a later run of another kind\nPASS team::runtime::live_native_lead_tests::a_teammate_cannot_plant 1s\n");
    await put(logs, "profile/team.runtime.live_claude_tests.under_stop_ends_its_command.log", "thread 'x' panicked at src/x.rs:1:1:\nRED-RUN-PROCESS-LIST\n");
    await put(logs, `local/${logName("launch_context::tests::a_local_turn_answers")}`, okLog("launch_context::tests::a_local_turn_answers"));
    await put(logs, `rerun/${logName("team::runtime::live_claude_tests::under_stop_ends_its_command")}`, okLog("team::runtime::live_claude_tests::under_stop_ends_its_command"));
    const runs = [
      { file: "summary-local.txt", commit: "afd69922", source: "local group on the stage", logs: "local" },
      { file: "summary-profile.txt", mtime: "20261104-155848", commit: "afd69922", source: "profile group on the stage", logs: "profile" },
      { file: "summary-rerun.txt", commit: "b2d2b49e", source: "under_stop rerun on test/live-ps-piped", logs: "rerun" },
    ];
    await put(logs, "runs.json", JSON.stringify(runs));
    const html = renderReport(await readLivePass(logs, { script, repo }));
    assert.deepEqual(liveRows(html), [
      ["local", "a_local_turn_answers", "PASS", "afd69922", "3 s"],
      ["profile", "under_stop_ends_its_command", "PASS", "b2d2b49e", "6 s"],
      ["profile", "a_teammate_cannot_plant", "BLOCKED", "afd69922", ""],
    ]);
    assert.ok(!html.includes("RED-RUN-PROCESS-LIST"), "a result a later run replaced shows no log");
    assert.ok(html.includes("2 passed, 0 failed, 1 blocked, 0 not run"), "summary.txt is not read when runs.json lists the runs");
    const cover = html.slice(0, html.indexOf('<section class="live">'));
    for (const run of runs) assert.ok(cover.includes(run.file) && cover.includes(run.commit) && cover.includes(run.source), run.file);
    for (const [bad, why] of [
      [[{ ...runs[0], file: "../summary-local.txt" }], "a file outside the folder"],
      [[{ ...runs[0], file: "notes.txt" }], "a file that is not a summary"],
      [[{ ...runs[0], commit: "afd6992g" }], "a commit that is not a hash"],
      [[{ ...runs[0], file: "summary-missing.txt" }], "a missing summary"],
      [[], "no runs"],
      [{ file: "summary-local.txt" }, "not a list"],
    ]) {
      await put(logs, "runs.json", JSON.stringify(bad));
      await assert.rejects(readLivePass(logs, { script, repo }), /report_live_runs_invalid/, why);
    }
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("D-M1: a PASS row counts as passed only when its own log shows that test passing; otherwise it reads UNPROVEN", async () => {
  const logs = await folder("live-unproven-");
  const repo = await folder("live-unproven-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    const names = ["proven", "no_log", "other_test_log", "no_result_line", "blocked_first"].map((name) => `a::tests::${name}`);
    await put(repo, "live-pass.sh", liveScript(names.map((name) => `local|lib|${name}|30|none`)));
    await put(logs, "summary.txt", [
      "== local: no account, no tokens",
      "PASS a::tests::proven 3s",
      "PASS a::tests::no_log 4s",
      "PASS a::tests::other_test_log 5s",
      "PASS a::tests::no_result_line 6s",
      "BLOCKED a::tests::blocked_first: BLOCKED: needs a profile",
      "",
    ].join("\n"));
    await put(logs, logName("a::tests::proven"), okLog("a::tests::proven"));
    await put(logs, logName("a::tests::other_test_log"), okLog("a::tests::proven"));
    await put(logs, logName("a::tests::no_result_line"), "running 1 test\ntest a::tests::no_result_line ... ok\n");
    const html = renderReport(await readLivePass(logs, { script, repo }));
    assert.deepEqual(liveRows(html), [
      ["local", "proven", "PASS", "", "3 s"],
      ["local", "no_log", "UNPROVEN", "", "4 s"],
      ["local", "other_test_log", "UNPROVEN", "", "5 s"],
      ["local", "no_result_line", "UNPROVEN", "", "6 s"],
      ["local", "blocked_first", "BLOCKED", "", ""],
    ]);
    assert.ok(html.includes("missing: a.tests.no_log.log"), "a missing log is named");
    assert.ok(html.includes("1 passed, 0 failed, 1 blocked, 3 unproven, 0 not run"), "an unproven PASS is not counted as passed");
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("D-M1: with runs.json each row reads its log from its own run's folder, never from another run or the folder root", async () => {
  const logs = await folder("live-bound-");
  const repo = await folder("live-bound-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    const local = "launch_context::tests::a_local_turn_answers";
    const device = "github::tests::device_sign_in";
    const stop = "team::runtime::live_claude_tests::under_stop_ends_its_command";
    const plant = "team::runtime::live_native_lead_tests::a_teammate_cannot_plant";
    await put(repo, "live-pass.sh", liveScript([`local|lib|${local}|30|none`, `keychain|lib|${device}|30|host`, `profile|lib|${stop}|60|profile`, `profile|lib|${plant}|60|profile`]));
    await put(logs, "summary-local.txt", `== local: no account, no tokens\nPASS ${local} 3s\n`);
    await put(logs, "summary-keychain.txt", `== keychain: temporary keychains\nFAIL ${device} 6s exit 101: ${logs}/keychain/${logName(device)}\n`);
    await put(logs, "summary-profile.txt", `== profile: holds a shared lock on the owner's profile\nFAIL ${stop} 8s exit 101: ${logs}/profile/${logName(stop)}\nPASS ${plant} 13s\n`);
    await put(logs, "summary-rerun.txt", `== profile: holds a shared lock on the owner's profile\nPASS ${stop} 6s\n`);
    await put(logs, `local/${logName(local)}`, okLog(local));
    await put(logs, `keychain/${logName(device)}`, "thread 'github::tests::device_sign_in' panicked at src/github.rs:240:5:\nassertion failed: keychain_run_fail\n");
    await put(logs, logName(device), "thread 'github::tests::device_sign_in' panicked at src/other.rs:1:1:\nassertion failed: root_log_fail\n");
    await put(logs, `profile/${logName(stop)}`, "thread 'x' panicked at src/x.rs:1:1:\nassertion failed: red_run_process_list\n");
    await put(logs, `rerun/${logName(stop)}`, okLog(stop));
    await put(logs, logName(plant), okLog(plant));
    await put(logs, `rerun/${logName(plant)}`, okLog(plant));
    const runs = [
      { file: "summary-local.txt", commit: "afd69922", source: "local group", logs: "local" },
      { file: "summary-keychain.txt", commit: "afd69922", source: "keychain group", logs: "keychain" },
      { file: "summary-profile.txt", commit: "afd69922", source: "profile group", logs: "profile" },
      { file: "summary-rerun.txt", commit: "b2d2b49e", source: "under_stop rerun", logs: "rerun" },
    ];
    await put(logs, "runs.json", JSON.stringify(runs));
    const html = renderReport(await readLivePass(logs, { script, repo }));
    assert.deepEqual(liveRows(html), [
      ["local", "a_local_turn_answers", "PASS", "afd69922", "3 s"],
      ["keychain", "device_sign_in", "FAIL", "afd69922", "6 s"],
      ["profile", "under_stop_ends_its_command", "PASS", "b2d2b49e", "6 s"],
      ["profile", "a_teammate_cannot_plant", "UNPROVEN", "afd69922", "13 s"],
    ]);
    assert.ok(html.includes("keychain_run_fail") && !html.includes("root_log_fail"), "a failure shows its own run's log, not the root log of the same name");
    assert.ok(!html.includes("red_run_process_list"), "a result a later run replaced shows no log of the earlier run");
    assert.ok(html.includes(`missing: profile/${logName(plant)}`), "a row whose run folder lacks its log names the missing log, though another run has one");
    assert.ok(html.includes("2 passed, 1 failed, 0 blocked, 1 unproven, 0 not run"));
    await mkdir(join(logs, "plain"), { recursive: true });
    await symlink(join(logs, "local"), join(logs, "linked"));
    const { logs: _dropped, ...unbound } = runs[0];
    for (const [bad, why] of [
      [[unbound], "a run without its log folder"],
      [[{ ...runs[0], logs: "../local" }], "a log folder outside the folder"],
      [[{ ...runs[0], logs: join(logs, "local") }], "an absolute log folder"],
      [[{ ...runs[0], logs: "summary-local.txt" }], "a log folder that is a file"],
      [[{ ...runs[0], logs: "linked" }], "a log folder that is a link"],
      [[{ ...runs[0], logs: "nowhere" }], "a missing log folder"],
    ]) {
      await put(logs, "runs.json", JSON.stringify(bad));
      await assert.rejects(readLivePass(logs, { script, repo }), /report_live_runs_invalid/, why);
    }
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("F-L1: two runs that name one log folder are refused, however the second spells it", async () => {
  const logs = await folder("live-shared-logs-");
  const repo = await folder("live-shared-logs-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    const name = "launch_context::tests::a_local_turn_answers";
    await put(repo, "live-pass.sh", liveScript([`local|lib|${name}|30|none`]));
    await put(logs, "summary-first.txt", `== local: no account, no tokens\nPASS ${name} 3s\n`);
    await put(logs, "summary-second.txt", `== local: no account, no tokens\nPASS ${name} 4s\n`);
    await put(logs, `local/inner/${logName(name)}`, okLog(name));
    await put(logs, `other/inner/${logName(name)}`, okLog(name));
    await symlink(join(logs, "local"), join(logs, "alias"));
    const run = (file, folderName) => ({ file, commit: "afd69922", source: file, logs: folderName });
    await put(logs, "runs.json", JSON.stringify([run("summary-first.txt", "local/inner"), run("summary-second.txt", "other/inner")]));
    assert.deepEqual(liveRows(renderReport(await readLivePass(logs, { script, repo }))).map((row) => row[2]), ["PASS"], "two runs with their own folders are read");
    for (const [second, why] of [["local/inner", "the same folder named twice"], ["alias/inner", "the same folder reached through a linked parent"]]) {
      await put(logs, "runs.json", JSON.stringify([run("summary-first.txt", "local/inner"), run("summary-second.txt", second)]));
      await assert.rejects(readLivePass(logs, { script, repo }), /report_live_runs_invalid: summary-second\.txt logs .*already/, why);
    }
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("PS-2: a compile-break excerpt keeps rustc's numbered source lines, and a macOS ps -ef row after it is still left out", async () => {
  const logs = await folder("live-rustc-");
  const repo = await folder("live-rustc-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    const name = "team::runtime::live_claude_tests::under_stop_ends";
    await put(repo, "live-pass.sh", liveScript([`profile|lib|${name}|60|profile`]));
    await put(logs, "summary.txt", `== profile: holds a shared lock\nFAIL ${name} 8s exit 101: ${logs}/${logName(name)}\n`);
    await put(logs, logName(name), [
      "   Compiling crazyagent v0.2.0",
      "error[E0308]: mismatched types",
      "  --> src/team/runtime.rs:12:5",
      "   |",
      "12 |     let x: u8 = code_line_mark();",
      "   |                 ^^^^^^^^^^^^^^^^ expected `u8`, found `u32`",
      "...",
      "140 |   fn second_line_mark() {",
      "",
      "UID   PID  PPID   C STIME   TTY           TIME CMD",
      "  501 41234     1   0  4:25PM ttys001    0:00.01 /usr/bin/argv_mark_four --print",
      "error: could not compile `crazyagent` (lib test) due to 1 previous error",
      "",
    ].join("\n"));
    const html = renderReport(await readLivePass(logs, { script, repo }));
    assert.ok(html.includes("12 |     let x: u8 = code_line_mark();") && html.includes("140 |   fn second_line_mark() {"), "rustc's numbered source lines stay");
    assert.ok(!html.includes("argv_mark_four") && html.includes("[process list left out: 2 lines]"), "the ps -ef header and row are left out");
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("D-M1: a failure excerpt leaves out process-list lines, so a dumped ps listing never reaches the report", async () => {
  const logs = await folder("live-ps-");
  const repo = await folder("live-ps-repo-");
  try {
    const script = join(repo, "live-pass.sh");
    const stop = "team::runtime::live_claude_tests::under_stop_ends";
    await put(repo, "live-pass.sh", liveScript([`profile|lib|${stop}|60|profile`]));
    await put(logs, "summary.txt", `== profile: holds a shared lock\nFAIL ${stop} 8s exit 101: ${logs}/${logName(stop)}\n`);
    await put(logs, logName(stop), [
      "running 1 test",
      `test ${stop} ... FAILED`,
      "",
      `---- ${stop} stdout ----`,
      "  PID  PPID COMMAND",
      "41234     1 /opt/homebrew/bin/claude --print --model argv_mark_one",
      `thread '${stop}' panicked at src/team/runtime.rs:10:5:`,
      "assertion failed: the background command ended with its task",
      "41240 41234 /bin/sh -c sleep 600 argv_mark_two",
      "41241 41240 sleep 600",
      "owner   41250   0.0  0.1 410000 12000 s001  S+   4:25PM   0:00.01 /usr/bin/argv_mark_three --verbose",
      "",
      "failures:",
      "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2990 filtered out; finished in 5.68s",
      "",
    ].join("\n"));
    const html = renderReport(await readLivePass(logs, { script, repo }));
    assert.deepEqual(liveRows(html).map((row) => row[2]), ["FAIL"]);
    assert.ok(html.includes("panicked at src/team/runtime.rs:10:5") && html.includes("assertion failed: the background command ended with its task"), "the panic and its message stay");
    assert.ok(html.includes("test result: FAILED. 0 passed; 1 failed"), "the cargo result line stays");
    for (const shown of ["argv_mark_one", "argv_mark_two", "argv_mark_three", "sleep 600", "PPID"]) assert.ok(!html.includes(shown), shown);
    assert.ok(html.includes("[process list left out: 2 lines]") && html.includes("[process list left out: 3 lines]"), "each left-out run of lines is marked");
  } finally {
    await rm(logs, { recursive: true });
    await rm(repo, { recursive: true });
  }
});

test("the retro mode reads trials 1 and 2 from the saved audit, plan, timeline, tokens and frames into one report", async () => {
  const root = await folder("retro-report-");
  try {
    await put(root, "trial-plan.md", "# DEC-104 trial 2026-10-28 (ONE run, hard gate)\n\nBuild 18.6, chore/b18-stage 3a7cc9be. Throwaway repo /Users/owner/Projects/ca-trial-1028.\n\n## Before\n- install.sh 3a7cc9be\n\n## Master message (one message, typed once)\nSpawn two Claude leads.\n\n## Checks (in order)\n1. Graph tree: master -> Lead A -> 2 teammates.\n2. Own worktree for each worker.\n\n## After\n- leader/trial-audit.sh <project> > leader/trial/audit.txt\n- Transcript and DB audit: ctx/graphify/Skill counts.\n");
    await put(root, "audit.txt", "FAIL 1 tree: team_agents rows=5 leads=2\nPASS 2 worktrees: agent 43 ok\nFAIL T1 transcripts: 40(master) files=1\nSUMMARY project=ca-trial-1028 pass=1 fail=2 unknown=0\n");
    await put(root, "timeline.txt", "21:44:30 trial start\n21:45:00\n21:46:01 master message sent\n21:52:49 stop all\n");
    await put(root, "tokens.txt", "agent 40 {'input': 14, 'output': 2027, 'cache_read': 231606, 'cache_create': 30448, 'turns': 7}\nagent 41 {'input': 8, 'output': 1119, 'cache_read': 126171, 'cache_create': 28948, 'turns': 4}\n");
    await put(root, "limits-before.txt", `at 2026-10-28 21:40:15\n('chatgpt-account-${SECRETS.chatgpt}', None)\n('claude-account-${SECRETS.account}', 'max', 3.0, 100.0)\n('claude-account-f2fac034ae8d7eb16c75', 'max', 27.0, 8.0)\n`);
    await put(root, "frames/f00000.txt", " [◆▪◆▪] CrazyAgent  /  ca-trial-1028  /  Team\n");
    await put(root, "frames/f00001.txt", ` Master  ${SECRETS.email}\n`);
    await put(root, "trial-1028.gif", GIF);
    await put(root, "trial-plan-2.md", "# DEC-104 second and final trial\n\nBuild 18.7 = chore/b18-stage after F1-F9.\n\n## Steps (TUI)\n1. Home: a, e, path ~/Projects/ca-trial-2.\n\n## Checks (each PASS/FAIL)\n4. For: Me question reaches Questions; I answer Hello. [not exercised]\n13. F8 TUI: terminal box renders cleanly. [border broken]\n\n## After\n- trial-audit.sh ~/Projects/ca-trial-2 > audit-2.txt\n");
    await put(root, "audit-2.txt", "PASS 4 owner question: For: Me questions to owner=1\n  4 detail line for check four\nUNKNOWN 13 tui: read the recorded frames\nSUMMARY project=ca-trial-2 pass=1 fail=0 unknown=1\n");
    await put(root, "timeline-2.txt", "23:50:47 trial-2 start\n23:54:39 answered Hello\n00:04:43 stop all\n");
    await put(root, "tokens-2.txt", "45 {'input': 10, 'output': 100, 'cache_read': 1000, 'cache_create': 50, 'turns': 2}\n  ('claude-account-f2fac034ae8d7eb16c75', 49.0, 23.0)\n");
    await put(root, "limits-before-2.txt", "at 2026-10-28 23:50:30\n('claude-account-f2fac034ae8d7eb16c75', 'max', 48.0, 22.0)\n");
    await put(root, "frames-2/f00000.txt", " [◆▪◆▪] CrazyAgent  /  ca-trial-2  /  Team\n");
    await put(root, "trial-2.gif", GIF);
    const model = await readRetro(root);
    assert.deepEqual(model.trials.map((trial) => [trial.trial, trial.build, trial.project, trial.duration, trial.checks.map((check) => `${check.id}:${check.result}`)]), [
      [1, "18.6 (3a7cc9be)", "ca-trial-1028", "8 min 19 s", ["1:FAIL", "2:PASS", "T1:FAIL"]],
      [2, "18.7", "ca-trial-2", "13 min 56 s", ["4:PASS", "13:UNKNOWN"]],
    ]);
    assert.deepEqual(model.trials[0].accounts, ["claude-subscription ×2", "codex-chatgpt ×1"]);
    assert.equal(model.trials[0].tokens.join(", "), "input 22, output 3146, cache read 357777, cache create 59396, turns 11 (2 agents)");
    assert.equal(model.trials[1].tokens.join(", "), "input 10, output 100, cache read 1000, cache create 50, turns 2 (1 agent)", "trial 2 lines have no agent prefix");
    const html = renderReport(model);
    assert.match(html, /<h1>Trials 1 and 2/);
    assert.ok(html.includes("Graph tree: master -&gt; Lead A -&gt; 2 teammates."), "proves comes from the plan");
    assert.ok(html.includes("first trial: not exercised"), "trial 2 checks name the trial 1 result they re-prove");
    assert.ok(html.includes("4 detail line for check four"), "continuation lines stay with their check");
    assert.ok(html.includes("leader/trial-audit.sh &lt;project&gt;"), "how comes from the plan");
    assert.ok(html.includes("Spawn two Claude leads."), "the master message is shown");
    assert.ok(html.includes("answered Hello"), "the timeline is shown");
    assert.ok(!html.includes("<th>Why</th><th>OK</th>"), "a steps table drops the columns no step fills");
    assert.equal(svgImages(html).length, 3, "every frame is drawn as an image");
    for (const value of [SECRETS.chatgpt, SECRETS.account, SECRETS.email]) assert.ok(![html, ...svgImages(html)].join("\n").includes(value));
    const images = html.slice(html.indexOf('<section class="images">'));
    assert.ok(images.includes("trial-1028.gif") && images.includes("trial-2.gif"));
    assert.ok(!html.includes("data:image/gif"), "a retro GIF is left out when its masked frames are drawn");
    assert.equal([...images.matchAll(/left out: /g)].length, 2, "each left-out GIF says why");
    const cover = html.slice(html.indexOf('<header class="cover">'), html.indexOf("</header>"));
    assert.ok(cover.includes("trial-1028.gif") && cover.includes("trial-2.gif"), "the cover names the left-out GIFs");
  } finally {
    await rm(root, { recursive: true });
  }
});

test("the command writes report.html and a real report.pdf printed by Google Chrome headless", { timeout: 120_000 }, async () => {
  const root = await trialFolder();
  const engine = await engineRoot([]);
  try {
    const printed = await new Promise((done, failed) => {
      execFile(process.execPath, [REPORT, "trial", root], { timeout: 110_000, env: { ...process.env, CRAZYAGENT_ENGINE_ROOT: engine } }, (error, stdout, stderr) => (error ? failed(new Error(`${error.message}\n${stderr}`)) : done(stdout.trim())));
    });
    assert.equal(printed, join(root, "report.pdf"));
    const html = await readFile(join(root, "report.html"), "utf8");
    assert.equal(assertMasked(html), html);
    const pdf = await readFile(join(root, "report.pdf"));
    assert.equal(pdf.subarray(0, 5).toString("latin1"), "%PDF-");
    assert.ok(pdf.length > 10_000, `a real document, ${pdf.length} bytes`);
    for (const file of ["report.html", "report.pdf"]) assert.equal((await stat(join(root, file))).mode & 0o777, 0o600, `${file} is private`);
  } finally {
    await rm(root, { recursive: true });
    await rm(engine, { recursive: true });
  }
});

test("PDF-1: Authorization schemes and every Cookie and Set-Cookie pair are masked mid-line, and so is a JWE token", async () => {
  const root = await trialFolder();
  try {
    await put(root, "logs/headers.log", `${HEADER_LINES}\n`);
    await addCheck(root, "T3-04", [{ kind: "log", path: "logs/headers.log", caption: "request headers" }]);
    const html = renderReport(await readTrial(root));
    assert.deepEqual(Object.entries(HEADERS).filter(([, value]) => html.includes(value)).map(([name]) => name), [], "no header value is shown");
    assert.ok(html.includes("curl -sS -H &#39;Authorization: [masked]&#39; https://api.example.org/v1"), "the mask stops at the closing quote");
    assert.ok(html.includes("request headers Cookie: [masked]"), "the words before the header stay");
    assert.equal(mask(mask(HEADER_LINES)), mask(HEADER_LINES), "masking twice changes nothing");
    assert.equal(assertMasked(html), html);
  } finally {
    await rm(root, { recursive: true });
  }
});

test("PDF-2: secrets wrapped across frame lines, encoded in logs or held in SVG text are masked or refused, and the residue check reads SVG text", async () => {
  const root = await trialFolder();
  try {
    await put(root, "frames/f00002.txt", `${WRAPPED_FRAME}\n`);
    await put(root, "logs/encoded.log", `${ENCODED_LINES}\n`);
    await put(root, "screenshots/03-entity.svg", svgText(`&#115;k-ant-api03-${ENCODED.entityTail}`));
    await put(root, "screenshots/04-split.svg", svgText(`<tspan>sk-ant-api03-</tspan><tspan>${ENCODED.tspanTail}</tspan>`));
    await put(root, "screenshots/05-plain.svg", svgText("Sign in page"));
    await addCheck(root, "T3-05", [
      { kind: "frame", path: "frames/f00002.txt", caption: "wrapped login" },
      { kind: "log", path: "logs/encoded.log", caption: "encoded values" },
      { kind: "screenshot", path: "screenshots/03-entity.svg", caption: "entity svg" },
      { kind: "screenshot", path: "screenshots/04-split.svg", caption: "split svg" },
      { kind: "screenshot", path: "screenshots/05-plain.svg", caption: "plain svg" },
    ]);
    const html = renderReport(await readTrial(root));
    const shown = [html, ...svgImages(html)].join("\n");
    assert.deepEqual([...WRAPPED_PARTS, ...Object.values(ENCODED)].filter((value) => shown.includes(value)), [], "no wrapped or encoded value is shown");
    assert.ok(svgImages(html).some((svg) => svg.includes("Login")), "the wrapped frame is still drawn");
    const check = section(html, "T3-05");
    assert.equal([...check.matchAll(/refused: /g)].length, 2, "the entity and the split SVG are refused");
    const images = html.slice(html.indexOf('<section class="images">'));
    assert.ok(images.includes("screenshots/05-plain.svg"), "an embedded SVG file is listed as unmasked");
    assert.equal(assertMasked(html), html);
    const svg = (body) => `<img src="data:image/svg+xml;base64,${Buffer.from(svgText(body)).toString("base64")}">`;
    assert.throws(() => assertMasked(svg(SECRETS.email)), /report_mask_residue/, "the residue check reads SVG text");
    assert.throws(() => assertMasked(svg(`&#115;k-ant-api03-${ENCODED.entityTail}`)), /report_mask_residue/, "the residue check decodes SVG entities");
    assert.throws(() => assertMasked(`<p>payload ${ENCODED.base64}</p>`), /report_mask_residue/, "the residue check reads base64");
  } finally {
    await rm(root, { recursive: true });
  }
});

test("PDF-3: Chrome prints with every network request refused, loopback included", { timeout: 120_000 }, async () => {
  const root = await folder("trial-report-network-");
  const asked = [];
  const server = createServer((request, response) => {
    asked.push(request.url);
    response.writeHead(200, { "content-type": "image/png" });
    response.end(png());
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  try {
    const { port } = server.address();
    await put(root, "probe.html", `<!doctype html><html><body><p>network probe</p><img src="http://127.0.0.1:${port}/loopback.png"><img src="http://localhost:${port}/named.png"></body></html>`);
    assert.equal(await printPdf(join(root, "probe.html"), join(root, "probe.pdf")), join(root, "probe.pdf"));
    assert.deepEqual(asked, [], "Chrome reached the network");
    const pdf = await readFile(join(root, "probe.pdf"));
    assert.equal(pdf.subarray(0, 5).toString("latin1"), "%PDF-");
    assert.equal((await stat(join(root, "probe.pdf"))).mode & 0o777, 0o600);
  } finally {
    server.closeAllConnections();
    await new Promise((done) => server.close(done));
    await rm(root, { recursive: true });
  }
});

test("PDF-6: a print that does not finish in time ends Chrome's whole process group, its helpers too, and never asks for a keychain", { timeout: 30_000 }, async () => {
  const root = await folder("trial-report-hang-");
  const stand = await hangingChrome(root);
  let pids = [];
  try {
    await put(root, "page.html", "<!doctype html><html><body><p>hang</p></body></html>");
    await assert.rejects(printPdf(join(root, "page.html"), join(root, "page.pdf"), { chrome: stand.chrome, timeout: 2_000 }), /report_pdf_failed/);
    pids = await standInPids(stand);
    assert.deepEqual(await stillRunning(pids), [], "no process of the print's group is left, the helper included");
    const argv = (await readFile(stand.argv, "utf8")).split("\n");
    assert.ok(argv.includes("--use-mock-keychain") && argv.includes("--password-store=basic"), "Chrome never asks macOS for a keychain");
    await assert.rejects(stat(join(root, "page.pdf")), { code: "ENOENT" }, "no PDF is left behind");
  } finally {
    for (const pid of await stillRunning(pids)) process.kill(pid, "SIGKILL");
    await rm(root, { recursive: true });
  }
});

test("PDF-7: a report command stopped by SIGTERM while Chrome prints ends Chrome's whole process group and still dies by the signal", { timeout: 60_000 }, async () => {
  const root = await trialFolder();
  const engine = await engineRoot([]);
  const tools = await folder("trial-report-stand-");
  const stand = await hangingChrome(tools);
  let pids = [];
  try {
    const cli = spawn(process.execPath, [REPORT, "trial", root, "--engine", engine, "--chrome", stand.chrome], { stdio: "ignore", env: { ...process.env, CRAZYAGENT_CHROME: "/usr/bin/false" } });
    let exited = false;
    const ended = new Promise((done) => cli.once("exit", (code, signal) => {
      exited = true;
      done(signal ?? code);
    }));
    pids = await standInPids(stand, () => exited);
    cli.kill("SIGTERM");
    assert.equal(await ended, "SIGTERM", "the command still dies by the signal it got");
    assert.deepEqual(await stillRunning(pids), [], "no process of the print's group outlives the command");
  } finally {
    for (const pid of await stillRunning(pids)) process.kill(pid, "SIGKILL");
    for (const path of [root, engine, tools]) await rm(path, { recursive: true });
  }
});

test("PDF-4: the cover names every image the report cannot mask, and account labels and home paths are masked in text and frames", async () => {
  const root = await trialFolder();
  try {
    await put(root, "frames/f00003.txt", "  │Account       Claude-AcmeWorks          │\n  │ cwd /Users/owner/Projects/ca-trial-3    │\n");
    await put(root, "logs/paths.log", "transcript ~/.claude/projects/-Users-owner-Projects-ca-trial-3/5f1c.jsonl\nconfig /home/owner/.codex/config.toml\nsigned in as Codex-PersonalPlus\n");
    await addCheck(root, "T3-06", [{ kind: "frame", path: "frames/f00003.txt", caption: "account label" }, { kind: "log", path: "logs/paths.log", caption: "paths" }]);
    const html = renderReport(await readTrial(root));
    const cover = html.slice(html.indexOf('<header class="cover">'), html.indexOf("</header>"));
    const missing = ["are not masked", "screenshots/01-open-project.png", "trial.gif"].filter((part) => !cover.includes(part));
    assert.deepEqual(missing, [], "the cover says plainly which images are not masked");
    const shown = [html, ...svgImages(html)].join("\n");
    assert.deepEqual(["AcmeWorks", "PersonalPlus", "/Users/owner", "-Users-owner-", "/home/owner"].filter((value) => shown.includes(value)), [], "no account label or home path is shown");
    assert.deepEqual(["Claude-[account]", "Codex-[account]", "~/Projects/ca-trial-3", "-Users-[user]-Projects-ca-trial-3", "~/.codex/config.toml"].filter((value) => !shown.includes(value)), [], "the masked forms keep their kind and path");
    assert.equal(assertMasked(html), html);
  } finally {
    await rm(root, { recursive: true });
  }
});

test("PDF-5: an image over the per-file cap and images past the total cap are refused and named", async () => {
  const root = await trialFolder();
  try {
    const filler = (mib) => Buffer.concat([png(), Buffer.alloc(mib * 1024 * 1024)]);
    await put(root, "screenshots/big.png", filler(16));
    for (const n of [1, 2, 3]) await put(root, `screenshots/wide-${n}.png`, filler(14));
    await addCheck(root, "T3-07", ["big", "wide-1", "wide-2", "wide-3"].map((name) => ({ kind: "screenshot", path: `screenshots/${name}.png`, caption: name })));
    const html = renderReport(await readTrial(root));
    const check = section(html, "T3-07");
    assert.deepEqual([...check.matchAll(/<img alt="([\w-]+)"/g)].map((match) => match[1]), ["wide-1", "wide-2"], "only the images under both caps are embedded");
    assert.match(check, /big <span class="path">[^<]*<\/span><\/figcaption><p class="problem">refused: larger than/);
    assert.match(check, /wide-3 <span class="path">[^<]*<\/span><\/figcaption><p class="problem">refused: the report already embeds/);
    const images = html.slice(html.indexOf('<section class="images">'));
    assert.deepEqual(["big", "wide-1", "wide-2", "wide-3"].filter((name) => images.includes(`screenshots/${name}.png`)), ["wide-1", "wide-2"]);
  } finally {
    await rm(root, { recursive: true });
  }
});

test("PDF2R2-1: the engine's account labels, the home folder and the user name are masked by value in every reader, frames included, whatever their shape", async () => {
  const known = { home: "/Users/jo-an", user: "jo-an" };
  const engine = await engineRoot(["acme research lab", "Claude-team blue", "Me"]);
  const root = await trialFolder();
  const logs = await folder("known-live-");
  const retro = await folder("known-retro-");
  const empty = await folder("known-no-engine-");
  const lines = [
    "pool order acme research lab, then Claude-team blue",
    "cwd /Users/jo-an/Projects/ca-trial-3",
    "transcript ~/.claude/projects/-Users-jo-an-Projects-ca-trial-3/5f1c.jsonl",
    "git author jo-an",
  ].join("\n");
  const shownOf = (html) => [html, ...svgImages(html)].join("\n").toLowerCase();
  const leaks = (html) => ["acme", "research", "team blue", "jo-an"].filter((value) => shownOf(html).includes(value));
  try {
    await put(root, "logs/known.log", `${lines}\n`);
    await put(root, "frames/f00004.txt", "  ┌─ Accounts ───────────┐\n  │ first acme research │\n  │ lab second          │\n  └─────────────────────┘\n");
    await addCheck(root, "T3-08", [{ kind: "log", path: "logs/known.log", caption: "known values" }, { kind: "frame", path: "frames/f00004.txt", caption: "wrapped label" }]);
    const html = renderReport(await readTrial(root, { engine, ...known }));
    assert.deepEqual(leaks(html), [], "no label, home name or user name is shown in the trial report");
    const missing = ["[account]", "~/Projects/ca-trial-3", "-Users-[user]-Projects-ca-trial-3", "git author [user]", "Masked by value: 2 account labels"].filter((part) => !shownOf(html).includes(part.toLowerCase()));
    assert.deepEqual(missing, [], "the masked forms and the cover note are shown");
    assert.ok(svgImages(html).some((svg) => svg.includes("second")), "the wrapped frame is still drawn");
    assert.throws(() => assertMasked("<p>ACME Research Lab</p>"), /report_mask_residue/, "the residue check knows the labels");

    const script = join(logs, "live-pass.sh");
    await put(logs, "live-pass.sh", "#!/bin/bash\nTESTS=$(\n  cat <<'EOF'\nlocal|lib|engine::accounts::tests::a_label_is_listed|30|host\nEOF\n)\n");
    await put(logs, "summary.txt", `== local: no tokens\nFAIL engine::accounts::tests::a_label_is_listed 3s exit 101: ${logs}/engine.accounts.tests.a_label_is_listed.log\n`);
    await put(logs, "engine.accounts.tests.a_label_is_listed.log", "thread 'a' panicked at src/a.rs:1:1:\nassertion failed: listed acme research lab for jo-an\n");
    assert.deepEqual(leaks(renderReport(await readLivePass(logs, { script, repo: empty, engine, ...known }))), [], "no label or user name is shown in the live report");

    await put(retro, "audit.txt", "FAIL 1 tree: acme research lab ran as jo-an\nSUMMARY project=ca-trial-1028 pass=0 fail=1 unknown=0\n");
    await put(retro, "audit-2.txt", "PASS 4 owner question: Claude-team blue answered\nSUMMARY project=ca-trial-2 pass=1 fail=0 unknown=0\n");
    assert.deepEqual(leaks(renderReport(await readRetro(retro, { engine, ...known }))), [], "no label or user name is shown in the retro report");

    await assert.rejects(main(["trial", root, "--engine", empty]), /BLOCKED: no engine database/, "without the engine's labels the command refuses to write a report");
  } finally {
    for (const path of [engine, root, logs, retro, empty]) await rm(path, { recursive: true });
  }
});
