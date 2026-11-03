import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { crc32, deflateSync } from "node:zlib";
import { assertMasked, mask, readLivePass, readRetro, readTrial, renderReport } from "./report.mjs";

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
    await put(logs, "claude_bare_loads_a_plugin_dir.log", "running 1 test\nPASS-LOG-BODY\ntest result: ok. 1 passed\n");
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
    assert.equal([...images.matchAll(/the recorded TUI text held values the report masked/g)].length, 1, "only trial 1's frames held a secret");
  } finally {
    await rm(root, { recursive: true });
  }
});

test("the command writes report.html and a real report.pdf printed by Google Chrome headless", { timeout: 120_000 }, async () => {
  const root = await trialFolder();
  try {
    const printed = await new Promise((done, failed) => {
      execFile(process.execPath, [REPORT, "trial", root], { timeout: 110_000 }, (error, stdout, stderr) => (error ? failed(new Error(`${error.message}\n${stderr}`)) : done(stdout.trim())));
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
  }
});
