import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { stopRelaunched } from "../../apps/desktop/test/native/trial-app.mjs";
import { MAPPING_FILES } from "./mapping-docs.mjs";

const TRIAL = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(TRIAL, "..", "..");
const DESKTOP = join(REPO, "apps", "desktop");
const PROMPT = join(TRIAL, "prompt.md");
const STOP_SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];
const REAP_MS = 10000;
const CLOSE_MS = 5000;
const POLL_MS = 200;
const READ_MS = 10000;
const TOOL_MS = 30000;
const AUDIT_MS = 10 * 60 * 1000;
const REPORT_MS = 10 * 60 * 1000;
const RENDER_MS = 5 * 60 * 1000;
const LINE_MAX = 1 << 20;
const GIF_LINES = 60;
const GIF_BATCH = 40;
const FOLDERS = ["frames", "logs", "db", "checks", "screenshots"];
const ORDER_ROWS = 3000;
const PEM_BEGIN = /-----BEGIN [A-Z ]*PRIVATE KEY-----/;
const PEM_END = /-----END [A-Z ]*PRIVATE KEY-----/;
const PEM_BLOCK = /-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----/g;
const SECRET_NAME = "(?:[A-Za-z0-9]+[-_.])*(?:token|secret|password|passwd|api[-_]?key|apikey|session[-_]?key|cookie|device[-_]code|user[-_]code|auth[-_]?code|access[-_]key|private[-_]key|(?:account|organization|org|user)[-_]?(?:id|uuid))";
const MASKS = [
  [/\bsk-[A-Za-z0-9][A-Za-z0-9_-]{15,}/g, "[key]"],
  [/\bsess-[A-Za-z0-9]{16,}/g, "[key]"],
  [/\bgh[pousr]_[A-Za-z0-9]{20,}/g, "[key]"],
  [/\bgithub_pat_[A-Za-z0-9_]{20,}/g, "[key]"],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/g, "[jwt]"],
  [/\bya29\.[A-Za-z0-9_-]{10,}/g, "[token]"],
  [/\b1\/\/[A-Za-z0-9_-]{10,}/g, "[token]"],
  [/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{8,}/gi, "$1 [masked]"],
  [new RegExp(`(${SECRET_NAME})(\\\\?["']?\\s*[:=]\\s*\\\\?["']?)[^\\s"'\\\\,;&}\\[\\])]+`, "gi"), "$1$2[masked]"],
  [/([?&](?:code|state|token|access_token|key)=)[^&\s"']+/gi, "$1[masked]"],
  [/(\b[a-z][a-z0-9+.-]*:\/\/[^\s:@/]+:)[^\s@/]+@/gi, "$1[masked]@"],
  [/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, "[email]"],
  [/\b((?:claude|chatgpt|codex|openai|anthropic)-account-)[0-9A-Za-z_-]+/gi, "$1[masked]"],
  [/(?<![-\w])[A-Z0-9]{4}-[A-Z0-9]{4,5}(?![-\w])/g, "[code]"],
];
const CHILD_ENV = /^(PATH|HOME|USER|LOGNAME|SHELL|LANG|TMPDIR|TERM|CRAZYAGENT_DESKTOP_SOCKET|LC_[A-Z0-9_]+|TRIAL[0-9]*_[A-Z0-9_]+|WD_[A-Z0-9_]+|V1_[A-Z0-9_]+)$/;
const SECRET_ENV = /KEY|TOKEN|SECRET|PASS|CRED|COOKIE|AUTH/i;
const LIVE = new Set();
const KINDS = new Map([
  ["claude/claude/native_managed", "claude-subscription"],
  ["codex/chatgpt/native_managed", "codex-chatgpt"],
  ["rcli/anthropic_api/api_key", "rcli-anthropic-api-key"],
  ["rcli/local/none", "rcli-local"],
]);

/** A refusal before the run touches anything: the runner prints it as BLOCKED and exits 2. */
class Refusal extends Error {}

/** Returns a line masker that remembers an open private key block across lines and masks only the block, keeping the text around it. Input: none. Output: a function from one line to its masked form. */
function lineMasker() {
  let key = false;
  return (line) => {
    let text = line;
    if (key) {
      const end = text.match(PEM_END);
      if (!end) return "[private key]";
      key = false;
      text = `[private key]${text.slice(end.index + end[0].length)}`;
    }
    text = text.replaceAll(PEM_BLOCK, "[private key]");
    const begin = text.match(PEM_BEGIN);
    if (begin) {
      key = true;
      text = `${text.slice(0, begin.index)}[private key]`;
    }
    return MASKS.reduce((masked, [pattern, value]) => masked.replace(pattern, value), text);
  };
}

/** Masks keys, tokens, Bearer and Basic values, cookies, device codes, emails and account ids. Input: any text. Output: the text with the same line count and each secret replaced by a marker. */
export function mask(text) {
  return String(text).split("\n").map(lineMasker()).join("\n");
}

/** Fills the trial prompt template with the absolute project path. Input: template text and the project path. Output: the rendered prompt; an unknown placeholder fails the assertion. */
export function renderPrompt(template, project) {
  const rendered = template.replaceAll("{{PROJECT}}", project);
  const left = rendered.match(/\{\{[^}]*\}\}/);
  assert.equal(left, null, `prompt placeholder ${left?.[0]} has no value`);
  return rendered;
}

/** Parses and validates the runner arguments. Input: argv after the script. Output: the options with resolved paths and command arrays; a bad argument throws a Refusal. */
export function options(argv) {
  const { values } = parseArgs({
    args: argv,
    strict: true,
    allowPositionals: false,
    options: {
      trial: { type: "string", default: "3" },
      build: { type: "string" },
      pane: { type: "string" },
      project: { type: "string" },
      evidence: { type: "string" },
      engine: { type: "string" },
      app: { type: "string", default: "/Applications/CrazyAgent.app" },
      "deadline-min": { type: "string", default: "120" },
      "frame-gap-s": { type: "string", default: "3" },
      "ps-after-s": { type: "string", default: "120" },
      "ps-every-s": { type: "string", default: "60" },
      "kill-grace-s": { type: "string", default: "20" },
      "frames-cmd": { type: "string" },
      "desktop-cmd": { type: "string" },
      "audit-cmd": { type: "string" },
      "report-cmd": { type: "string" },
    },
  });
  const trial = Number(values.trial);
  if (!Number.isInteger(trial) || trial < 1 || trial > 99) throw new Refusal(`--trial must be a whole number from 1 to 99, got ${values.trial}`);
  if (!/^[0-9a-f]{8}$/.test(values.build ?? "")) throw new Refusal("--build must be the stage sha8 the app was built from");
  const engine = absolute("--engine", values.engine ?? process.env.CRAZYAGENT_ENGINE_ROOT ?? join(homedir(), "Library", "Application Support", "CrazyAgent", "engine"));
  const project = absolute("--project", values.project ?? join(projectsHome(engine), `ca-trial-${trial}`));
  const evidence = absolute("--evidence", values.evidence ?? process.env.TRIAL_EVIDENCE ?? "");
  const app = absolute("--app", values.app);
  if (values["frames-cmd"] === undefined && !/^[A-Za-z0-9:._-]{1,64}$/.test(values.pane ?? "")) throw new Refusal("--pane must name the herdr pane that shows the TUI");
  const mark = `TRIAL${trial}-PROJECT-RULE`;
  const commands = {
    frames: command("--frames-cmd", values["frames-cmd"], ["herdr", "pane", "read", values.pane ?? ""]),
    desktop: command("--desktop-cmd", values["desktop-cmd"], [process.execPath, join(DESKTOP, "node_modules", "@wdio", "cli", "bin", "wdio.js"), "run", join(DESKTOP, "test", "native", `trial${trial}.wdio.conf.mjs`)]),
    audit: command("--audit-cmd", values["audit-cmd"], ["python3", "-I", join(TRIAL, "audit.py"), project, "--engine", engine, "--evidence", evidence, "--project-mark", mark]),
    report: command("--report-cmd", values["report-cmd"], [process.execPath, join(TRIAL, "report.mjs"), "trial", evidence]),
  };
  const needed = [PROMPT, join(engine, "engine.db"), join(app, "Contents", "Info.plist")];
  if (values["desktop-cmd"] === undefined) needed.push(commands.desktop[1], commands.desktop[3]);
  if (values["audit-cmd"] === undefined) needed.push(commands.audit[2]);
  if (values["report-cmd"] === undefined) needed.push(commands.report[1]);
  for (const path of needed) if (!existsSync(path)) throw new Refusal(`missing ${path}`);
  return {
    trial,
    build: values.build,
    project,
    evidence,
    engine,
    app,
    mark,
    commands,
    desktopCwd: values["desktop-cmd"] === undefined ? DESKTOP : REPO,
    deadlineMs: seconds("--deadline-min", values["deadline-min"], 24 * 60) * 60000,
    frameGapMs: seconds("--frame-gap-s", values["frame-gap-s"], 600) * 1000,
    psAfterMs: seconds("--ps-after-s", values["ps-after-s"], 24 * 3600) * 1000,
    psEveryMs: seconds("--ps-every-s", values["ps-every-s"], 3600) * 1000,
    graceMs: seconds("--kill-grace-s", values["kill-grace-s"], 600) * 1000,
  };
}

/** Reads the owner's projects folder, where the app lists projects, from the engine store, read only. Input: the engine root. Output: the folder; a store without one throws a Refusal. */
function projectsHome(engine) {
  if (!existsSync(join(engine, "engine.db"))) throw new Refusal(`missing ${join(engine, "engine.db")}`);
  let row;
  try {
    const db = readOnly(engine);
    try {
      row = db.prepare("SELECT path FROM projects_home WHERE singleton=1").get();
    } finally {
      db.close();
    }
  } catch (error) {
    throw new Refusal(`the engine has no projects folder (${error.message}); set it in the app or pass --project`);
  }
  if (typeof row?.path !== "string" || row.path.length === 0) throw new Refusal("the engine has no projects folder; set it in the app or pass --project");
  return row.path;
}

/** Resolves one path argument and requires it to be absolute. Input: the flag name and its text. Output: the normalized path; a relative or empty one throws a Refusal. */
function absolute(flag, text) {
  if (!text || !isAbsolute(text)) throw new Refusal(`${flag} must be an absolute path, got "${text}"`);
  return resolve(text);
}

/** Reads one positive number argument. Input: the flag name, its text and the largest value allowed. Output: the number; anything else throws a Refusal. */
function seconds(flag, text, max) {
  const value = Number(text);
  if (!Number.isFinite(value) || value <= 0 || value > max) throw new Refusal(`${flag} must be a number above 0 and at most ${max}, got ${text}`);
  return value;
}

/** Reads one command override as a JSON array of strings, or keeps the default. Input: the flag name, its JSON text and the default argv. Output: the argv; a malformed override throws a Refusal. */
function command(flag, text, fallback) {
  if (text === undefined) return fallback;
  let argv;
  try {
    argv = JSON.parse(text);
  } catch {
    throw new Refusal(`${flag} must be a JSON array of strings`);
  }
  if (!Array.isArray(argv) || argv.length === 0 || !argv.every((part) => typeof part === "string" && part.length > 0)) throw new Refusal(`${flag} must be a JSON array of strings`);
  return argv;
}

/** Refuses a run that would reuse the project folder or a run folder that already holds evidence. Input: the options. Output: none; a reused folder throws a Refusal before anything is written. */
function refuseReuse(opts) {
  if (existsSync(opts.project)) throw new Refusal(`${opts.project} already exists; the runner never deletes it, move it aside or pass --project`);
  if (existsSync(opts.evidence)) {
    if (!statSync(opts.evidence).isDirectory()) throw new Refusal(`${opts.evidence} is not a folder`);
    if (readdirSync(opts.evidence).length > 0) throw new Refusal(`${opts.evidence} is not empty; every trial run gets a fresh run folder`);
  }
}

/** Opens the steps.jsonl writer of a run folder; each step takes the next line number so desktop spec lines interleave in order, and a failed write counts as a failed step instead of throwing. Input: the run folder. Output: an object with add and a running ok flag. */
function stepWriter(folder) {
  const path = join(folder, "steps.jsonl");
  let failed = 0;
  return {
    add(surface, title, why, ok, detail, evidence = []) {
      const prior = existsSync(path) ? readFileSync(path, "utf8").split("\n").filter(Boolean).length : 0;
      const present = evidence.filter((relative) => existsSync(join(folder, relative)));
      const step = { n: prior + 1, surface, title, why, at: new Date().toISOString(), ok, detail: mask(detail), evidence: present };
      if (!ok) failed += 1;
      try {
        appendFileSync(path, `${JSON.stringify(step)}\n`);
      } catch (error) {
        failed += 1;
        console.error(mask(`steps.jsonl: ${error.message}`));
      }
      console.log(`step ${step.n} ${surface} ${title}: ${ok ? "ok" : "FAILED"} ${step.detail}`);
    },
    get ok() {
      return failed === 0;
    },
  };
}

/** Runs git in a folder with no system or global config, no prompts, a fixed trial identity and a deadline, and fails on a non-zero exit. Input: the folder and git arguments. Output: stdout text. */
function git(folder, args) {
  const env = { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: process.env.HOME ?? folder, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" };
  const result = spawnSync("git", ["-c", "user.name=CrazyAgent Trial", "-c", "user.email=trial@invalid", ...args], { cwd: folder, env, encoding: "utf8", timeout: TOOL_MS, killSignal: "SIGKILL" });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.error?.message ?? result.stderr.trim()}`);
  return result.stdout;
}

/** The seeded order rows: a header and ORDER_ROWS deterministic id,item,price,quantity lines, enough that printing them all is a large output. Input: none. Output: the CSV text. */
function orders() {
  const rows = ["id,item,price,quantity"];
  for (let id = 1; id <= ORDER_ROWS; id += 1) rows.push(`${id},item-${String(id % 40).padStart(2, "0")},${1 + ((id * 37) % 90)}.${String((id * 53) % 100).padStart(2, "0")},${1 + (id % 4)}`);
  return `${rows.join("\n")}\n`;
}

/** Seeds the trial project: one commit on main with the trial rule, a tiny Rust crate whose cargo test agents run, a small Python shop package whose call chain a codebase question follows, an order report whose full output is large and the mapping checks' research documents and secret-named decoy, then the land-on branch and an untracked hidden folder. Input: the project path and trial number. Output: the seed sha and a log of what git reports. */
export function seed(project, trial) {
  mkdirSync(project, { recursive: true });
  const files = {
    ".gitignore": ".crazyagent/\nprivate/\ntarget/\nCargo.lock\n",
    "CLAUDE.md": `# ca-trial-${trial} rules\n\nTRIAL${trial}-PROJECT-RULE: every Python function has a one-line docstring.\n`,
    "Cargo.toml": `[package]\nname = "ca-trial-${trial}"\nversion = "0.1.0"\nedition = "2021"\n`,
    "README.md": `# ca-trial-${trial}\n\nThrowaway repo for the CrazyAgent DEC-112 trial ${trial}.\n`,
    "calc.py": "def placeholder():\n    \"\"\"Placeholder.\"\"\"\n    return None\n",
    "src/lib.rs": "/// Doubles a number. Input: the number. Output: twice it.\npub fn double(x: i32) -> i32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    "shop/money.py": "from decimal import ROUND_HALF_UP, Decimal\n\n\ndef round_cents(amount):\n    \"\"\"Rounds an amount to whole cents, halves up.\"\"\"\n    return float(Decimal(str(amount)).quantize(Decimal(\"0.01\"), rounding=ROUND_HALF_UP))\n",
    "shop/tax.py": "TAX_RATE = 0.08\n\n\ndef add_tax(amount, rate=TAX_RATE):\n    \"\"\"Returns the amount with tax added, unrounded.\"\"\"\n    return amount * (1 + rate)\n",
    "shop/pricing.py": "from shop.money import round_cents\nfrom shop.tax import add_tax\n\n\ndef line_amount(price, quantity):\n    \"\"\"Returns the untaxed amount of one order line.\"\"\"\n    return price * quantity\n\n\ndef order_total(rows):\n    \"\"\"Returns the taxed total of (price, quantity) rows.\"\"\"\n    return round_cents(add_tax(sum(line_amount(price, quantity) for price, quantity in rows)))\n",
    "shop/report.py": "from shop.pricing import order_total\n\n\ndef total_line(rows):\n    \"\"\"Returns the printed TOTAL line of (price, quantity) rows.\"\"\"\n    return f\"TOTAL {order_total(rows):.2f}\"\n",
    "data/orders.csv": orders(),
    "report.sh": "#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\nexec awk -F, 'NR > 1 { amount = $3 * $4; total += amount; printf \"%s %s %.2f\\n\", $1, $2, amount } END { printf \"TOTAL %.2f\\n\", total }' data/orders.csv\n",
    ...MAPPING_FILES,
  };
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(project, name)), { recursive: true });
    writeFileSync(join(project, name), text);
  }
  git(project, ["init", "-q", "-b", "main"]);
  git(project, ["add", "--", ...Object.keys(files)]);
  git(project, ["commit", "-q", "-m", "Initial trial repo"]);
  git(project, ["branch", `trial${trial}-land`]);
  mkdirSync(join(project, "private"));
  writeFileSync(join(project, "private", "note.txt"), `TRIAL${trial}-HIDDEN-MARK: agents must not read this file.\n`);
  const sha = git(project, ["rev-parse", "HEAD"]).trim();
  const log = [
    `seed ${sha}`,
    git(project, ["branch", "--list", "-v", "--no-abbrev"]).trimEnd(),
    git(project, ["ls-files"]).trimEnd(),
    git(project, ["status", "--porcelain", "--ignored"]).trimEnd(),
  ].join("\n");
  return { sha, log };
}

/** Opens the engine database read-only with query_only set. Input: the engine root. Output: an open DatabaseSync the caller closes. */
function readOnly(engine) {
  const db = new DatabaseSync(join(engine, "engine.db"), { readOnly: true });
  db.exec("PRAGMA query_only=ON");
  return db;
}

/** Sums every usage_tokens row of the engine. Input: the engine root. Output: row count, per-column sums and their total. */
function usage(engine) {
  const db = readOnly(engine);
  try {
    const row = db.prepare("SELECT COUNT(*) AS rows, COALESCE(SUM(input), 0) AS input, COALESCE(SUM(output), 0) AS output, COALESCE(SUM(cache_read), 0) AS cache_read, COALESCE(SUM(cache_write), 0) AS cache_write FROM usage_tokens").get();
    return { rows: row.rows, input: row.input, output: row.output, cache_read: row.cache_read, cache_write: row.cache_write, total: row.input + row.output + row.cache_read + row.cache_write };
  } finally {
    db.close();
  }
}

/** Names each connected account by kind only, never by id or email. Input: the engine root. Output: kinds in account order, such as claude-subscription or codex-chatgpt. */
function accountKinds(engine) {
  const db = readOnly(engine);
  try {
    const rows = db.prepare("SELECT json_extract(CAST(body AS TEXT), '$.connection.harness') AS harness, json_extract(CAST(body AS TEXT), '$.details.provider') AS provider, json_extract(CAST(body AS TEXT), '$.connection.auth.method') AS method FROM accounts ORDER BY seq").all();
    return rows.map((row) => {
      const key = [row.harness, row.provider, row.method].map((part) => (typeof part === "string" && /^[a-z0-9_]{1,32}$/.test(part) ? part : "unknown")).join("/");
      return KINDS.get(key) ?? key.replaceAll(/[/_]/g, "-");
    });
  } finally {
    db.close();
  }
}

/** Reads the app's short version string with plutil. Input: the app bundle path. Output: the version, or "unknown" when plutil cannot read it. */
function appVersion(app) {
  const result = spawnSync("plutil", ["-extract", "CFBundleShortVersionString", "raw", join(app, "Contents", "Info.plist")], { encoding: "utf8", timeout: TOOL_MS, killSignal: "SIGKILL" });
  const version = result.status === 0 ? result.stdout.trim() : "";
  return /^[0-9A-Za-z.+-]{1,32}$/.test(version) ? version : "unknown";
}

/** Runs a short command with a time limit and collects its output. Input: argv and the limit in ms. Output: exit code (null when it could not start or was killed), stdout and stderr. */
function exec(argv, ms) {
  return new Promise((done) => {
    const child = spawn(argv[0], argv.slice(1), { stdio: ["ignore", "pipe", "pipe"], timeout: ms, killSignal: "SIGKILL" });
    let out = "";
    let err = "";
    child.stdout.setEncoding("utf8").on("data", (chunk) => { out += chunk; });
    child.stderr.setEncoding("utf8").on("data", (chunk) => { err += chunk; });
    child.once("error", (error) => done({ code: null, out, err: error.message }));
    child.once("close", (code) => done({ code, out, err }));
  });
}

/** Opens one output stream of a log file: its own line buffer and key-block state, so stdout and stderr chunks never mix inside a line before masking. A failed write is reported once and later text is dropped. Input: the log path, created by the caller, and the failure callback. Output: write and end functions for streamed text. */
function maskedStream(path, onError) {
  const line = lineMasker();
  let rest = "";
  let broken = false;
  const flush = (text) => {
    if (broken) return;
    try {
      assert.ok(text.endsWith("\n"), "a flush holds whole lines only");
      appendFileSync(path, `${text.slice(0, -1).split("\n").map(line).join("\n")}\n`);
    } catch (error) {
      broken = true;
      onError(error);
    }
  };
  return {
    write(chunk) {
      rest += chunk;
      const cut = rest.lastIndexOf("\n");
      if (cut >= 0) {
        flush(rest.slice(0, cut + 1));
        rest = rest.slice(cut + 1);
      } else if (rest.length > LINE_MAX) {
        flush(`${rest}\n`);
        rest = "";
      }
    },
    end() {
      if (rest) flush(`${rest}\n`);
      rest = "";
    },
  };
}

/** Tells whether a process group still has a member. Input: the group id. Output: true while one exists. */
function groupAlive(pgid) {
  try {
    process.kill(-pgid, 0);
    return true;
  } catch (error) {
    if (error.code === "ESRCH") return false;
    if (error.code === "EPERM") return true;
    throw error;
  }
}

/** Sends a signal to a process group, ignoring a group that is already gone. Input: the group id and signal. Output: none. */
function signalGroup(pgid, signal) {
  try {
    process.kill(-pgid, signal);
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

/** Last resort when the runner process exits: SIGKILL every supervised group still recorded as live, so no desktop run outlives its supervisor. Input: none. Output: none. */
function killLive() {
  for (const pgid of LIVE) {
    try {
      process.kill(-pgid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") console.error(`could not stop group ${pgid}: ${error.code}`);
    }
  }
}

/** Stops every supervised group still live, reporting a group that cannot be stopped instead of throwing. Input: the SIGKILL grace in ms. Output: none. */
async function stopLive(graceMs) {
  for (const pgid of [...LIVE]) {
    try {
      await stopGroup(pgid, graceMs);
    } catch (error) {
      console.error(mask(`could not stop group ${pgid}: ${error.message}`));
    }
    LIVE.delete(pgid);
  }
}

/** Builds the environment of every supervised child: the allowlisted names of the runner's own environment, minus any name that looks like a credential, plus the given TRIAL_* values. Input: the extra values. Output: the environment object. */
function childEnv(extra) {
  const env = {};
  for (const [name, value] of Object.entries(process.env)) if (CHILD_ENV.test(name) && !SECRET_ENV.test(name)) env[name] = value;
  return { ...env, ...extra };
}

/** Stops a process group: SIGTERM, then SIGKILL after the grace period. Input: the group id and the grace in ms. Output: null when the group was already empty, else how it was stopped, naming members that outlived SIGKILL. */
async function stopGroup(pgid, graceMs) {
  assert.ok(Number.isFinite(graceMs) && graceMs > 0, "every stop has a SIGKILL grace");
  if (!groupAlive(pgid)) return null;
  signalGroup(pgid, "SIGTERM");
  const grace = Date.now() + graceMs;
  while (groupAlive(pgid) && Date.now() < grace) await pause(POLL_MS);
  let how = "SIGTERM";
  if (groupAlive(pgid)) {
    signalGroup(pgid, "SIGKILL");
    how = `SIGKILL after the ${graceMs / 1000} s grace`;
  }
  const reap = Date.now() + REAP_MS;
  while (groupAlive(pgid) && Date.now() < reap) await pause(POLL_MS);
  return groupAlive(pgid) ? `${how}, members still present` : how;
}

/** Runs one long command in its own process group with a deadline and a stop signal, streaming masked output to a log; the group is recorded as live until it is stopped. Input: argv, working folder, environment, log path, deadline in ms, SIGKILL grace in ms, an AbortSignal and the failure callback for log writes. Output: exit code, signal, cause ("deadline", or "stop" with the stop reason), how the runner stopped the group and any members left after exit, and elapsed ms. */
async function supervise(argv, { cwd, env, log, ms, graceMs, stop, onError }) {
  writeFileSync(log, "");
  const out = maskedStream(log, onError);
  const err = maskedStream(log, onError);
  const started = Date.now();
  const child = spawn(argv[0], argv.slice(1), { cwd, env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
  if (child.pid !== undefined) LIVE.add(child.pid);
  const closed = new Promise((done) => child.once("close", done));
  const exited = new Promise((done) => {
    child.once("exit", (code, signal) => done({ code, signal }));
    child.once("error", (error) => done({ code: null, signal: null, error: error.message }));
  });
  child.stdout.setEncoding("utf8").on("data", out.write);
  child.stderr.setEncoding("utf8").on("data", err.write);
  const timer = new AbortController();
  const deadline = pause(ms, "deadline", { signal: timer.signal }).catch(() => null);
  const stopped = new Promise((done) => {
    if (stop.aborted) done("stop");
    else stop.addEventListener("abort", () => done("stop"), { once: true });
  });
  const first = await Promise.race([exited, deadline, stopped]);
  const cause = typeof first === "string" ? first : null;
  const how = cause && child.pid !== undefined ? await stopGroup(child.pid, graceMs) : null;
  const result = cause ? await exited : first;
  timer.abort();
  const stray = child.pid !== undefined ? await stopGroup(child.pid, graceMs) : null;
  LIVE.delete(child.pid);
  await Promise.race([closed, pause(CLOSE_MS)]);
  if (result.error) err.write(`could not start ${argv[0]}: ${result.error}\n`);
  out.end();
  err.end();
  return { ...result, cause, reason: cause === "stop" ? stop.reason : null, stopped: how, stray, ms: Date.now() - started };
}

/** Describes how a supervised command ended. Input: its result and the deadline in ms. Output: one line for a step detail. */
function ending(result, ms) {
  const minutes = (result.ms / 60000).toFixed(1);
  const parts = [];
  const group = result.stopped ? `process group stopped with ${result.stopped}` : "process group already empty";
  if (result.cause === "deadline") parts.push(`deadline of ${+(ms / 60000).toFixed(2)} min passed; ${group}`);
  else if (result.cause === "stop") parts.push(`${result.reason instanceof Error ? "runner failure" : `interrupted by ${result.reason}`}; ${group}`);
  else if (result.error) parts.push(`could not start: ${result.error}`);
  else if (result.signal) parts.push(`killed by ${result.signal}`);
  else parts.push(`exit ${result.code}`);
  parts.push(`after ${minutes} min`);
  if (result.stray) parts.push(`processes left in its group were stopped with ${result.stray}`);
  return parts.join(", ");
}

/** Starts the TUI recorder: reads the pane every gap, keeps only frames that changed, masked, as frames/fNNNNN.txt, and takes a last frame on stop. Input: the frames argv, the run folder and the gap in ms. Output: the first read result plus stop(), which resolves to the frame and failure counts. */
async function startRecorder(argv, evidence, gapMs) {
  const halt = new AbortController();
  const state = { frames: 0, failed: 0 };
  let last = null;
  const tick = async () => {
    try {
      const read = await exec(argv, READ_MS);
      if (read.code !== 0) {
        state.failed += 1;
        return false;
      }
      if (read.out !== last) {
        last = read.out;
        writeFileSync(join(evidence, "frames", `f${String(state.frames).padStart(5, "0")}.txt`), mask(read.out));
        state.frames += 1;
      }
      return true;
    } catch (error) {
      state.failed += 1;
      console.error(mask(`frame read failed: ${error.message}`));
      return false;
    }
  };
  const first = await tick();
  const loop = (async () => {
    while (!halt.signal.aborted) {
      await pause(gapMs, null, { signal: halt.signal }).catch(() => null);
      await tick();
    }
  })();
  return {
    first,
    async stop() {
      halt.abort();
      await loop;
      return state;
    },
  };
}

/** Lists the processes of the trial: those whose command names the app, the project or the engine root, and all their descendants. Input: ps text with pid, ppid, pgid and command, and the marker strings. Output: "pid pgid command" lines. */
function trialProcesses(text, markers) {
  const rows = [];
  for (const line of text.split("\n")) {
    const match = line.match(/^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/);
    if (match) rows.push({ pid: Number(match[1]), ppid: Number(match[2]), pgid: Number(match[3]), command: match[4] });
  }
  const keep = new Set(rows.filter((row) => markers.some((marker) => row.command.includes(marker))).map((row) => row.pid));
  let grew = true;
  while (grew) {
    grew = false;
    for (const row of rows) {
      if (!keep.has(row.pid) && keep.has(row.ppid)) {
        keep.add(row.pid);
        grew = true;
      }
    }
  }
  return rows.filter((row) => keep.has(row.pid)).map((row) => `${row.pid} ${row.pgid} ${row.command}`);
}

/** Starts the process snapshots for launch argv evidence: masked logs/ps-NN.txt after a first delay, then at a fixed interval. Input: the run folder, the marker strings, the delay and the interval in ms. Output: stop(), which resolves to the snapshot count. */
function startSnapshots(evidence, markers, afterMs, everyMs) {
  const halt = new AbortController();
  let count = 0;
  const loop = (async () => {
    let wait = afterMs;
    while (!halt.signal.aborted) {
      const waited = await pause(wait, true, { signal: halt.signal }).catch(() => false);
      if (!waited) break;
      wait = everyMs;
      try {
        const ps = await exec(["ps", "-axww", "-o", "pid=,ppid=,pgid=,command="], READ_MS);
        if (ps.code !== 0) continue;
        count += 1;
        writeFileSync(join(evidence, "logs", `ps-${String(count).padStart(2, "0")}.txt`), mask(`${trialProcesses(ps.out, markers).join("\n")}\n`));
      } catch (error) {
        console.error(mask(`process snapshot failed: ${error.message}`));
      }
    }
  })();
  return {
    async stop() {
      halt.abort();
      await loop;
      return count;
    },
  };
}

/** Draws one frame as a square SVG of its last lines, white on dark grey Menlo; square because qlmanage renders a square thumbnail and scales anything else. Input: frame text. Output: SVG text with the lines in its top 1000 pixels. */
function frameSvg(text) {
  const escape = (line) => line.replaceAll(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  const lines = text.split("\n").slice(-GIF_LINES);
  const rows = lines.map((line, index) => `<text x="8" y="${16 + index * 16}" font-family="Menlo" font-size="13" fill="#dddddd" xml:space="preserve">${escape(line)}</text>`);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1600"><rect width="100%" height="100%" fill="#1e1e1e"/>${rows.join("")}</svg>`;
}

/** Builds trial.gif from the recorded frames: SVG per frame, qlmanage to PNG, then ffmpeg crops the top 1600x1000 and writes a palette GIF at one frame a second. Input: the run folder. Output: ok and a detail line. */
async function buildGif(evidence) {
  const frames = readdirSync(join(evidence, "frames")).filter((name) => /^f\d{5}\.txt$/.test(name)).sort();
  if (frames.length === 0) return { ok: false, detail: "no frames to render" };
  const work = mkdtempSync(join(tmpdir(), "ca-trial-gif-"));
  try {
    const svgs = frames.map((name) => {
      const path = join(work, name.replace(/\.txt$/, ".svg"));
      writeFileSync(path, frameSvg(readFileSync(join(evidence, "frames", name), "utf8")));
      return path;
    });
    const pngs = svgs.map((path) => `${path}.png`);
    for (let index = 0; index < svgs.length; index += GIF_BATCH) {
      const render = await exec(["qlmanage", "-t", "-s", "1600", "-o", work, ...svgs.slice(index, index + GIF_BATCH)], RENDER_MS);
      if (render.code !== 0) return { ok: false, detail: `qlmanage failed: ${render.err.trim()}` };
    }
    const missing = pngs.filter((path) => !existsSync(path));
    if (missing.length > 0) return { ok: false, detail: `qlmanage rendered ${pngs.length - missing.length} of ${pngs.length} frames` };
    const gif = join(evidence, "trial.gif");
    const encode = await exec(["ffmpeg", "-loglevel", "error", "-y", "-framerate", "1", "-pattern_type", "glob", "-i", join(work, "f*.svg.png"), "-vf", "crop=1600:1000:0:0,scale=1200:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse", gif], RENDER_MS);
    if (encode.code !== 0 || !existsSync(gif)) return { ok: false, detail: `ffmpeg failed: ${encode.err.trim()}` };
    if (readFileSync(gif).subarray(0, 4).toString("latin1") !== "GIF8") return { ok: false, detail: "ffmpeg wrote something other than a GIF" };
    return { ok: true, detail: `${frames.length} frames, ${statSync(gif).size} bytes` };
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

/** Reads token usage into db/tokens-<when>.json and records the step. Input: options, run folder, step writer and "before" or "after". Output: the total, or null when the engine could not be read. */
function recordUsage(opts, steps, when) {
  const why = when === "before" ? "the report shows what the trial cost from this baseline" : "the report shows what the trial cost against the baseline";
  try {
    const read = usage(opts.engine);
    writeFileSync(join(opts.evidence, "db", `tokens-${when}.json`), `${JSON.stringify(read, null, 2)}\n`);
    steps.add("engine", `Read token usage ${when} the run`, why, true, `${read.total} tokens in ${read.rows} usage rows`, [`db/tokens-${when}.json`]);
    return read.total;
  } catch (error) {
    steps.add("engine", `Read token usage ${when} the run`, why, false, `engine.db: ${error.message}`);
    return null;
  }
}

/** Runs the trial: seed, recorder, WebdriverIO run with a deadline, manifest, then audit and report. A runner failure stops the desktop group and the recorder, is recorded in the manifest and a step, and the audit and report still run. Input: argv after the script. Output: the exit code (0 every step ok, 1 a step or the runner failed, 2 refused, 130 interrupted). */
export async function main(argv) {
  process.umask(0o077);
  let opts;
  try {
    opts = options(argv);
    refuseReuse(opts);
  } catch (error) {
    if (!(error instanceof Refusal) && !String(error.code).startsWith("ERR_PARSE_ARGS_")) throw error;
    console.error(mask(`BLOCKED: ${error.message}`));
    return 2;
  }
  const halt = new AbortController();
  const crash = new AbortController();
  const fail = (error) => {
    console.error(mask(`runner failure: ${error?.stack ?? error}`));
    if (!crash.signal.aborted) crash.abort(error instanceof Error ? error : new Error(String(error)));
  };
  const onSignal = (signal) => {
    if (!halt.signal.aborted) halt.abort(signal);
  };
  for (const signal of STOP_SIGNALS) process.on(signal, onSignal);
  process.on("uncaughtException", fail);
  process.on("unhandledRejection", fail);
  process.on("exit", killLive);
  const stop = AbortSignal.any([halt.signal, crash.signal]);
  for (const folder of FOLDERS) mkdirSync(join(opts.evidence, folder), { recursive: true });
  const started = new Date();
  const steps = stepWriter(opts.evidence);
  const accounts = (() => {
    try {
      return accountKinds(opts.engine);
    } catch (error) {
      console.error(mask(`accounts: ${error.message}`));
      return [];
    }
  })();
  const tokens = { before: null, after: null };
  let project = opts.project;
  let crashReported = false;
  const finish = () => {
    const manifest = { trial: opts.trial, build: opts.build, app_version: appVersion(opts.app), started: started.toISOString(), ended: new Date().toISOString(), project, accounts, tokens };
    if (crash.signal.aborted) manifest.failure = mask(crash.signal.reason.message);
    try {
      writeFileSync(join(opts.evidence, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    } catch (error) {
      console.error(mask(`manifest.json: ${error.message}`));
    }
  };
  const interrupted = () => {
    if (!halt.signal.aborted) return false;
    steps.add("engine", "Run interrupted", "the owner stopped the runner, so the audit and report did not run", false, `stopped by ${halt.signal.reason}`);
    if (!existsSync(join(opts.evidence, "manifest.json"))) finish();
    return true;
  };
  const reportCrash = () => {
    if (!crash.signal.aborted || crashReported) return;
    crashReported = true;
    steps.add("engine", "Runner failed", "a runner failure stopped the desktop run; the audit and report still run on what exists", false, crash.signal.reason.message);
  };

  try {
    const seeded = seed(opts.project, opts.trial);
    project = realpathSync(opts.project);
    writeFileSync(join(opts.evidence, "logs", "seed.log"), mask(`${seeded.log}\n`));
    steps.add("engine", "Seed the trial project", `every check runs on a fresh repo with the ${opts.mark} rule, the land-on branch and the hidden private folder`, true, `${project} at ${seeded.sha}, branches main and trial${opts.trial}-land, private/ untracked`, ["logs/seed.log"]);
  } catch (error) {
    steps.add("engine", "Seed the trial project", "every check needs the seeded project", false, error.message);
    finish();
    return 1;
  }
  const env = childEnv({ TRIAL_EVIDENCE: opts.evidence, TRIAL_PROJECT: project, TRIAL_PROMPT: PROMPT, TRIAL_ENGINE: opts.engine, TRIAL_LAND_ON: `trial${opts.trial}-land`, TRIAL_CLI: join(opts.app, "Contents", "MacOS", "crazyagent") });
  const envNames = Object.keys(env).sort();
  let recorder = null;
  let snapshots = null;
  let desktop = null;
  let shots = 0;
  let recorded = { frames: 0, failed: 0 };
  try {
    writeFileSync(join(opts.evidence, "logs", "prompt.txt"), renderPrompt(readFileSync(PROMPT, "utf8"), project));
    tokens.before = recordUsage(opts, steps, "before");
    if (interrupted()) return 130;
    recorder = await startRecorder(opts.commands.frames, opts.evidence, opts.frameGapMs);
    steps.add("tui", "Start the TUI recorder", "the TUI check reads the frames and the GIF the owner would have seen", recorder.first, recorder.first ? "first frame read" : "the first pane read failed; the recorder keeps trying", ["frames/f00000.txt"]);
    writeFileSync(join(opts.evidence, "logs", "env.txt"), `${envNames.join("\n")}\n`);
    snapshots = startSnapshots(opts.evidence, [opts.app, project, opts.engine], opts.psAfterMs, opts.psEveryMs);
    if (!stop.aborted) desktop = await supervise(opts.commands.desktop, { cwd: opts.desktopCwd, env, log: join(opts.evidence, "logs", "desktop.log"), ms: opts.deadlineMs, graceMs: opts.graceMs, stop, onError: fail });
  } catch (error) {
    fail(error);
  } finally {
    await stopLive(opts.graceMs);
    if (snapshots) shots = await snapshots.stop();
    if (recorder) recorded = await recorder.stop();
  }
  const relaunched = await stopRelaunched(opts.evidence).catch((error) => ({ failed: error.message }));
  if (relaunched) {
    const failed = typeof relaunched === "object";
    steps.add("desktop", "Quit the relaunched app", "an app the trial opened as the Dock does has no WebDriver service or process group to stop it", !failed, failed ? `could not stop it: ${relaunched.failed}` : relaunched, ["relaunched.json"]);
  }
  const desktopOk = desktop !== null && desktop.code === 0 && desktop.cause === null && !desktop.error;
  const desktopDetail = desktop === null ? "not started" : `${ending(desktop, opts.deadlineMs)}; ${shots} process snapshots; ${envNames.length} environment names in logs/env.txt`;
  steps.add("desktop", "WebdriverIO run ended", "the desktop spec drives every owner action; its exit says whether it reached the end", desktopOk, desktopDetail, ["logs/desktop.log", "logs/env.txt"]);
  const frameNames = [0, recorded.frames - 1].filter((index) => index >= 0).map((index) => `frames/f${String(index).padStart(5, "0")}.txt`);
  steps.add("tui", "Stop the TUI recorder", "the frames are the TUI evidence", recorded.frames > 0, `${recorded.frames} frames, ${recorded.failed} failed reads`, [...new Set(frameNames)]);
  if (interrupted()) return 130;
  reportCrash();

  const gif = await buildGif(opts.evidence).catch((error) => ({ ok: false, detail: error.message }));
  steps.add("tui", "Build the TUI GIF", "the report shows the TUI as one animation", gif.ok, gif.detail, ["trial.gif"]);
  tokens.after = recordUsage(opts, steps, "after");
  finish();
  if (interrupted()) return 130;

  const audit = await supervise(opts.commands.audit, { cwd: REPO, env, log: join(opts.evidence, "logs", "audit.log"), ms: AUDIT_MS, graceMs: opts.graceMs, stop: halt.signal, onError: fail }).catch((error) => ({ code: null, error: error.message, cause: null, ms: 0 }));
  const audited = audit.cause === null && (audit.code === 0 || audit.code === 1);
  const verdict = audit.code === 0 ? "no FAIL" : audit.code === 1 ? "at least one FAIL, see checks/" : "the audit could not run";
  steps.add("engine", "Run the audit", "the checks decide what the trial proved", audited, `${ending(audit, AUDIT_MS)}: ${verdict}`, ["logs/audit.log"]);
  if (interrupted()) return 130;

  const report = await supervise(opts.commands.report, { cwd: REPO, env, log: join(opts.evidence, "logs", "report.log"), ms: REPORT_MS, graceMs: opts.graceMs, stop: halt.signal, onError: fail }).catch((error) => ({ code: null, error: error.message, cause: null, ms: 0 }));
  steps.add("engine", "Build the report", "the owner reads the trial as one report", report.code === 0 && report.cause === null && !report.error, ending(report, REPORT_MS), ["logs/report.log", "report.html", "report.pdf"]);
  if (interrupted()) return 130;
  reportCrash();
  return steps.ok && !crash.signal.aborted ? 0 : 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (error) => {
      console.error(mask(error?.stack ?? String(error)));
      process.exit(1);
    },
  );
}
