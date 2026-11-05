import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { chmod, lstat, mkdtemp, open, readdir, readFile, realpath, rename, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir, userInfo } from "node:os";
import { basename, dirname, extname, isAbsolute, join, relative, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath, pathToFileURL } from "node:url";

const TEXT_BYTES = 64 * 1024;
const LOG_BYTES = 8 * 1024 * 1024;
const IMAGE_BYTES = 15 * 1024 * 1024;
const EMBED_BYTES = 40 * 1024 * 1024;
const SOURCE_BYTES = 2 * 1024 * 1024;
const EXCERPT_LINES = 30;
const FRAME_LINES = 120;
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const RESULTS = ["PASS", "FAIL", "UNKNOWN"];
const IMAGES = { ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp", ".svg": "image/svg+xml" };
const ACCOUNT_KINDS = { claude: "claude-subscription", chatgpt: "codex-chatgpt" };
const FLAGGED = "the recorded TUI text held values the report masked; this image may show them";
const LEFT_OUT = "left out: the masked frames show the same screens, and the GIF would show account labels and home paths unmasked";
const USAGE = "report.mjs trial <trial folder> | live <live-pass folder> --script <live-pass.sh> [--repo <checkout>] | retro <leader/trial folder>, each with [--engine <engine root>] [--out <folder>] [--chrome <executable>]";
const ENGINE = process.env.CRAZYAGENT_ENGINE_ROOT || join(homedir(), "Library", "Application Support", "CrazyAgent", "engine");
const KNOWN_MIN = 3;
const RUNS_MAX = 20;
const RUN_FILE = /^summary[A-Za-z0-9_.-]*\.txt$/;
const RUN_COMMIT = /^[0-9a-f]{7,40}$/;
const RUN_SOURCE_MAX = 300;
const RUN_LOGS = /^[A-Za-z0-9_][A-Za-z0-9_.-]*(?:\/[A-Za-z0-9_][A-Za-z0-9_.-]*){0,3}$/;
const RUN_LOGS_MAX = 200;
const PS_LINES = [/\bPID\b.*\b(?:COMMAND|CMD|ARGS|COMM)\b/, /^\s*\d{1,7}\s+(?!\|)\S/, /^\S+\s+\d{1,7}\s+\d+\.\d+\s+\d+\.\d+\s/];
const RETRO = [
  { trial: 1, audit: "audit.txt", plan: "trial-plan.md", timeline: "timeline.txt", tokens: "tokens.txt", limits: "limits-before.txt", frames: "frames", gif: "trial-1028.gif" },
  { trial: 2, audit: "audit-2.txt", plan: "trial-plan-2.md", timeline: "timeline-2.txt", tokens: "tokens-2.txt", limits: "limits-before-2.txt", frames: "frames-2", gif: "trial-2.gif" },
];
const SECRET_KEYS = "token|secret|password|passwd|api[_-]?key|apikey|access[_-]?key|private[_-]?key|cookie|session[_-]?key|authorization|credential|device[_-]?code|user[_-]?code|code[_-]?verifier|account[_-]?(?:id|uuid)|org(?:anization)?[_-]?(?:id|uuid)|user[_-]?(?:id|uuid)";
const RULES = [
  [/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, "[masked-private-key]"],
  [/\b([a-z][a-z0-9+.-]*:\/\/)[^\s/:@]+:[^\s/@]+@/gi, "$1[masked]@"],
  [/(\b(?:set-)?cookie["']?[ \t]*[:=])(?![ \t]*["']?\[masked\])([ \t]*["']?)[^\n"'\\]+/gi, "$1$2[masked]"],
  [/(\b(?:proxy-)?authorization["']?[ \t]*[:=])(?![ \t]*["']?\[masked\])([ \t]*["']?)[^\n"'\\]+/gi, "$1$2[masked]"],
  [/\bsk-ant-[A-Za-z0-9_-]{6,}/g, "sk-ant-[masked]"],
  [/\bsk-[A-Za-z0-9_-]{16,}/g, "sk-[masked]"],
  [/\b(gh[pousr]_)[A-Za-z0-9]{16,}/g, "$1[masked]"],
  [/\bgithub_pat_[A-Za-z0-9_]{16,}/g, "github_pat_[masked]"],
  [/\bxox[abposr]-[A-Za-z0-9-]{10,}/g, "xox-[masked]"],
  [/\bAKIA[0-9A-Z]{16}\b/g, "AKIA[masked]"],
  [/\bAIza[0-9A-Za-z_-]{30,}/g, "AIza[masked]"],
  [/\bya29\.[A-Za-z0-9._-]{10,}/g, "ya29.[masked]"],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*(?:\.[A-Za-z0-9_-]*){0,2}/g, "[masked-jwt]"],
  [/\b(Bearer)(\s+)([A-Za-z0-9._~+/=-]{8,})/gi, (all, scheme, space, value) => (/\d/.test(value) || value.length >= 20 ? `${scheme}${space}[masked]` : all)],
  [
    new RegExp(`(?<q>["']?)(?<key>[A-Za-z0-9_.-]*?(?:${SECRET_KEYS}))\\k<q>(?<sep>\\s*[:=]\\s*)(?!["']?\\[masked)(?<value>"[^"\\n]*"|'[^'\\n]*'|(?:\\[masked(?:-[a-z]+)?\\]|[^\\s"',;}\\[\\])])+)`, "gi"),
    (...args) => {
      const { q, key, sep, value } = args.at(-1);
      const wrap = value[0] === '"' || value[0] === "'" ? value[0] : "";
      return `${q}${key}${q}${sep}${wrap}[masked]${wrap}`;
    },
  ],
  [/([?&](?:code|state|code_challenge)=)(?!\[masked\])[^&\s#"']+/g, "$1[masked]"],
  [/\b((?:claude|chatgpt|codex|openai|anthropic|gemini)-account)-[A-Za-z0-9]{6,}/gi, "$1-[masked]"],
  [/\bacct_[A-Za-z0-9]{6,}/g, "acct_[masked]"],
  [/[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}/g, "[email]"],
  [/\b[A-Z0-9]{4}-[A-Z0-9]{4,5}\b/g, (code) => (/[A-Z]/.test(code) ? "[masked-code]" : code)],
  [/[\\/](?:Users|home)[\\/][^\\/\s"'<>:|─-╿…]+/g, "~"],
  [/-(Users|home)-[A-Za-z0-9._]+(?=-)/g, "-$1-[user]"],
  [/\b(Claude|Codex|ChatGPT|OpenAI|Gemini)-(?!Code\b)[A-Z][A-Za-z0-9]*/g, "$1-[account]"],
  [/(?<![A-Za-z0-9+/_-])[A-Za-z0-9+/_-]{24,}={0,2}(?![A-Za-z0-9+/_=-])/g, (run) => encoded(Buffer.from(run, "base64").toString("latin1"), run, true)],
  [/[^\s"'<>()[\]]*%[0-9A-Fa-f]{2}[^\s"'<>()[\]]*/g, (run) => encoded(run.replace(/\+/g, " ").replace(/%([0-9A-Fa-f]{2})/g, (all, hex) => String.fromCharCode(parseInt(hex, 16))), run, false)],
  [/^.*(?:\\u[0-9A-Fa-f]{4}|\\["/]).*$/gm, (line) => {
    const plain = line.replace(/\\u([0-9A-Fa-f]{4})/g, (all, hex) => String.fromCharCode(parseInt(hex, 16))).replace(/\\(["/\\])/g, "$1");
    const masked = mask(plain);
    return masked === plain ? line : masked;
  }],
];
const OPAQUE = /(?<![A-Za-z0-9_-])(?=[A-Za-z0-9_-]*\d)(?=[A-Za-z0-9_-]*[A-Za-z])[A-Za-z0-9_][A-Za-z0-9_-]{31,}(?![A-Za-z0-9_-])/g;

let KNOWN = { rules: [], note: "" };

function literal(value) {
  const units = [...value].filter((unit) => !/\s/.test(unit)).map((unit) => unit.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return new RegExp(`(?<![A-Za-z0-9\\[])${units.join("\\s*")}(?![A-Za-z0-9\\]])`, "gi");
}

function accountLabels(engine) {
  const path = join(engine, "engine.db");
  let db;
  try {
    db = new DatabaseSync(path, { readOnly: true });
    db.exec("PRAGMA query_only=ON");
    return db.prepare("SELECT json_extract(CAST(body AS TEXT), '$.details.name') AS name FROM accounts").all().map((row) => row.name).filter((name) => typeof name === "string");
  } catch (error) {
    return fail(`report_engine_unreadable: ${error.message.split("\n")[0]}`);
  } finally {
    db?.close();
  }
}

/**
 * Builds the mask rules for the values this machine knows: the engine's account labels, the home folder,
 * its encoded form in transcript paths and the user name, matched whatever their shape and across wraps.
 * Input: { engine, home, user }; without engine no labels are read. Output: { rules, note } for KNOWN and the cover.
 */
export function knownValues({ engine, home = homedir(), user = userInfo().username } = {}) {
  if (typeof home !== "string" || typeof user !== "string" || (engine !== undefined && typeof engine !== "string")) fail("report_known_invalid");
  const labels = engine === undefined ? null : accountLabels(engine);
  const longest = (values) => [...new Map(values.map((value) => [value.trim().toLowerCase(), value.trim()])).values()].sort((left, right) => right.length - left.length);
  const usable = longest(labels ?? []).filter((label) => label.replace(/\s/g, "").length >= KNOWN_MIN);
  const short = longest(labels ?? []).length - usable.length;
  const names = longest([user, basename(home)]).filter((name) => name.length >= KNOWN_MIN);
  const rules = [];
  if (home.length >= 2 && home !== dirname(home)) {
    const encode = (path) => path.replace(/[^A-Za-z0-9]/g, "-");
    rules.push([literal(home), "~"], [literal(encode(home)), `${encode(dirname(home))}-[user]`]);
  }
  for (const label of usable) rules.push([literal(label), "[account]"]);
  for (const name of names) rules.push([literal(name), "[user]"]);
  const parts = [labels === null ? "Masked by value: the home folder and the user name. No engine was read, so account labels are masked by shape only." : `Masked by value: ${usable.length} account label${usable.length === 1 ? "" : "s"} from the engine, the home folder and the user name.`];
  if (short) parts.push(`${short} label${short === 1 ? " is" : "s are"} shorter than ${KNOWN_MIN} characters and masked by shape only.`);
  if (!names.length) parts.push(`The user name is shorter than ${KNOWN_MIN} characters and masked by value only inside the home folder.`);
  return { rules, note: parts.join(" ") };
}
const BOX = /[─-╿]+/g;
const WRAPPED = "•";
const CSS = `
@page { size: Letter; margin: 14mm 12mm; }
:root { --ink: #1d232a; --muted: #5d6873; --line: #d5dbe1; --wash: #f4f6f8; --ok: #1b7f3b; --bad: #b3261e; --warn: #8a6100; --idle: #7d8590; }
* { box-sizing: border-box; }
body { margin: 0; color: var(--ink); background: #ffffff; font: 10.5pt/1.45 -apple-system, "Helvetica Neue", Arial, sans-serif; }
h1 { font-size: 22pt; margin: 0 0 4pt; }
h2 { font-size: 16pt; margin: 0 0 8pt; padding-bottom: 3pt; border-bottom: 1.5pt solid var(--ink); }
h3 { font-size: 12.5pt; margin: 16pt 0 6pt; }
h4 { font-size: 11pt; margin: 10pt 0 4pt; }
.cover { break-after: page; }
.cover h2 { margin-top: 16pt; }
.sharing { margin-top: 12pt; padding: 2pt 10pt 6pt; border: 1.5pt solid var(--warn); border-radius: 3pt; }
.sharing h2 { margin-top: 6pt; border-bottom: 0; }
.trial, .images, .live { break-before: page; }
.generated, .note, .path, .source { color: var(--muted); font-size: 9pt; font-weight: 400; }
dl { display: grid; grid-template-columns: max-content 1fr; gap: 2pt 12pt; margin: 6pt 0; }
dt { font-weight: 600; }
dd { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
table { width: 100%; border-collapse: collapse; margin: 6pt 0; font-size: 9.5pt; }
th, td { border-bottom: 0.75pt solid var(--line); padding: 3pt 5pt; text-align: left; vertical-align: top; overflow-wrap: anywhere; }
th { background: var(--wash); }
tr { break-inside: avoid; }
.result { display: inline-block; padding: 0 5pt; border-radius: 3pt; font-weight: 700; font-size: 8.5pt; color: #ffffff; background: var(--idle); white-space: nowrap; }
.result.pass { background: var(--ok); }
.result.fail { background: var(--bad); }
.result.unknown, .result.blocked, .result.unproven { background: var(--warn); }
.check { border-top: 0.75pt solid var(--line); margin-top: 12pt; padding-top: 4pt; }
figure { margin: 6pt 0; break-inside: avoid; }
figcaption { font-weight: 600; font-size: 9.5pt; margin-bottom: 2pt; }
pre { margin: 0; padding: 5pt 6pt; background: var(--wash); border: 0.75pt solid var(--line); border-radius: 3pt; font: 8pt/1.35 Menlo, monospace; white-space: pre-wrap; overflow-wrap: anywhere; }
img { max-width: 100%; height: auto; border: 0.75pt solid var(--line); }
.problem { color: var(--bad); }
blockquote { margin: 4pt 0; padding: 4pt 8pt; border-left: 3pt solid var(--line); background: var(--wash); white-space: pre-wrap; }
code { font: 8.5pt Menlo, monospace; overflow-wrap: anywhere; }
`;

function fail(message) {
  throw new Error(message);
}

/**
 * Masks secrets in one text: API keys, sk-ant-/sk- keys, Bearer values, OAuth and session tokens,
 * device codes, cookies, emails and account ids, also inside base64, URL-encoded and JSON-escaped forms.
 * Input: any value, read as a string. Output: the text with each secret replaced by a [masked] marker;
 * masking twice changes nothing.
 */
export function mask(value) {
  let out = String(value ?? "");
  for (const [pattern, replacement] of [...KNOWN.rules, ...RULES]) out = out.replace(pattern, replacement);
  return out;
}

function encoded(decoded, run, printable) {
  if (printable && !/^[\t\n\r\x20-\x7e]+$/.test(decoded)) return run;
  return mask(decoded) === decoded ? run : "[masked-encoded]";
}

function escape(value) {
  return String(value).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

function decode(value) {
  return value.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");
}

function entities(value) {
  const named = { lt: "<", gt: ">", quot: '"', apos: "'", amp: "&" };
  return value.replace(/&(?:#(\d{1,7})|#[xX]([0-9A-Fa-f]{1,6})|(lt|gt|quot|apos|amp));/g, (all, decimal, hex, name) => {
    if (name) return named[name];
    const point = decimal ? Number(decimal) : parseInt(hex, 16);
    return point <= 0x10ffff ? String.fromCodePoint(point) : all;
  });
}

function svgViews(svg) {
  const bare = svg.replace(/data:[\w+./-]+;base64,[A-Za-z0-9+/=\s]*/g, "data:");
  return [entities(bare), entities(bare.replace(/<\/text\s*>/gi, "\n").replace(/<[^>]*>/g, ""))];
}

function text(value) {
  return escape(mask(value ?? ""));
}

/**
 * Checks a finished report: its text and the text of every embedded SVG, entities decoded and split
 * text runs joined, must hold nothing the mask would change. Raster images are not read.
 * Input: the report HTML. Output: the same HTML. Throws report_mask_residue on any residue.
 */
export function assertMasked(html) {
  const views = [];
  const page = String(html).replace(/data:([\w+./-]+);base64,([A-Za-z0-9+/=]*)/g, (all, type, body) => {
    if (type === "image/svg+xml") views.push(...svgViews(Buffer.from(body, "base64").toString("utf8")));
    return "data:";
  });
  for (const view of [decode(page), ...views]) if (mask(view) !== view) fail("report_mask_residue");
  return html;
}

async function place(root, path) {
  if (typeof path !== "string" || path === "" || path.includes("\0") || isAbsolute(path) || path.split(/[\\/]/).includes("..")) {
    return { problem: "refused: not a plain path inside the run folder" };
  }
  const full = join(root, path);
  let info;
  try {
    info = await lstat(full);
  } catch (error) {
    if (error.code === "ENOENT" || error.code === "ENOTDIR") return { problem: `missing: ${path}` };
    throw error;
  }
  if (info.isSymbolicLink()) return { problem: "refused: a link" };
  if (!info.isFile()) return { problem: "refused: not a regular file" };
  const inner = relative(root, await realpath(full));
  if (inner === "" || inner.startsWith("..") || isAbsolute(inner)) return { problem: "refused: outside the run folder" };
  return { full, bytes: info.size };
}

async function readBounded(full, limit) {
  const handle = await open(full, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const buffer = Buffer.alloc(Math.min((await handle.stat()).size, limit) + 1);
    let used = 0;
    while (used < buffer.length) {
      const { bytesRead } = await handle.read(buffer, used, buffer.length - used, used);
      if (bytesRead === 0) break;
      used += bytesRead;
    }
    return { bytes: buffer.subarray(0, Math.min(used, limit)), truncated: used > limit };
  } finally {
    await handle.close();
  }
}

async function optionalText(root, path, limit = TEXT_BYTES * 4) {
  const where = await place(root, path);
  if (where.problem) return "";
  return (await readBounded(where.full, limit)).bytes.toString("utf8");
}

async function readJson(root, path, code) {
  const where = await place(root, path);
  if (where.problem) fail(`${code}: ${where.problem}`);
  const { bytes } = await readBounded(where.full, TEXT_BYTES * 16);
  try {
    return JSON.parse(bytes.toString("utf8"));
  } catch {
    return fail(`${code}: ${path}`);
  }
}

function dataUri(type, bytes) {
  return `data:${type};base64,${bytes.toString("base64")}`;
}

function frameMask(value) {
  return mask(value).replace(OPAQUE, "[masked]");
}

function frameChains(lines) {
  const width = Math.max(0, ...lines.map((line) => line.length));
  const open = new Map();
  const chains = [];
  for (const [row, line] of lines.entries()) {
    const seen = new Set();
    let from = 0;
    for (const cut of [...line.matchAll(BOX), { index: line.length, 0: "" }]) {
      const segment = line.slice(from, cut.index);
      const body = segment.trim();
      if (body) {
        const full = segment.length - segment.trimEnd().length <= 1 && (cut[0] !== "" || line.length >= width - 1);
        const part = { row, col: from + segment.length - segment.trimStart().length, body, full };
        const chain = open.get(from);
        if (chain?.at(-1).full) {
          chain.push(part);
        } else {
          if (chain) chains.push(chain);
          open.set(from, [part]);
        }
        seen.add(from);
      }
      from = cut.index + cut[0].length;
    }
    for (const [key, chain] of open) {
      if (seen.has(key)) continue;
      chains.push(chain);
      open.delete(key);
    }
  }
  return [...chains, ...open.values()].filter((chain) => chain.length > 1);
}

function joinedResidue(lines) {
  return frameChains(lines).some((chain) => {
    const joined = chain.map((part) => part.body).join("");
    return frameMask(joined) !== joined;
  });
}

/**
 * Masks one recorded TUI frame. A pane's lines are joined as the TUI wrapped them, so a secret cut across
 * lines is blotted out where it lies; each line is then masked, long opaque runs included.
 * Input: the frame text. Output: { lines, changed, residue }; residue means a value survived and the frame must not be shown.
 */
export function maskFrame(raw) {
  const lines = String(raw ?? "").replace(/\s+$/, "").split("\n").slice(0, FRAME_LINES);
  const cells = lines.map((line) => line.split(""));
  for (const chain of frameChains(lines)) {
    const joined = chain.map((part) => part.body).join("");
    const at = chain.flatMap((part) => Array.from({ length: part.body.length }, (unit, index) => [part.row, part.col + index]));
    for (const pattern of [...KNOWN.rules, ...RULES].map(([rule]) => rule).concat(OPAQUE)) {
      for (const match of joined.matchAll(pattern)) {
        const end = match.index + match[0].length - 1;
        if (!match[0] || at[match.index][0] === at[end][0] || frameMask(match[0]) === match[0]) continue;
        for (let index = match.index; index <= end; index += 1) cells[at[index][0]][at[index][1]] = WRAPPED;
      }
    }
  }
  const masked = cells.map((row) => frameMask(row.join("")));
  const residue = masked.some((line) => frameMask(line) !== line) || joinedResidue(masked);
  return { lines: masked, changed: masked.join("\n") !== lines.join("\n"), residue };
}

function frameSvg(lines) {
  const columns = Math.max(40, ...lines.map((line) => [...line].length));
  const width = Math.ceil(16 + columns * 7.9);
  const height = 16 + lines.length * 16;
  const rows = lines.map((line, index) => `<text x="8" y="${16 + index * 16}" font-family="Menlo, monospace" font-size="13" fill="#dddddd" xml:space="preserve">${escape(line)}</text>`);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}"><rect width="100%" height="100%" fill="#1e1e1e"/>${rows.join("")}</svg>`;
}

function pretty(raw) {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

function overCap(report, bytes) {
  return report.embedded + bytes > EMBED_BYTES ? `refused: the report already embeds ${report.embedded} bytes of images; the total cap is ${EMBED_BYTES}` : "";
}

async function evidence(root, item, report, trial) {
  const kind = String(item?.kind ?? "log");
  const path = String(item?.path ?? "");
  const shown = { kind, path, caption: String(item?.caption ?? path) };
  const where = await place(root, path);
  if (where.problem) return { ...shown, problem: where.problem };
  const type = IMAGES[extname(path).toLowerCase()];
  if (type && where.bytes > IMAGE_BYTES) return { ...shown, problem: `refused: larger than ${IMAGE_BYTES} bytes` };
  if (type && overCap(report, where.bytes)) return { ...shown, problem: overCap(report, where.bytes) };
  const { bytes, truncated } = await readBounded(where.full, type ? IMAGE_BYTES : TEXT_BYTES);
  if (type === "image/svg+xml" && svgViews(bytes.toString("utf8")).some((view) => mask(view) !== view)) {
    return { ...shown, problem: "refused: the SVG holds text the report would mask, and SVG markup cannot be masked reliably" };
  }
  if (type) {
    if (overCap(report, bytes.length)) return { ...shown, problem: overCap(report, bytes.length) };
    report.embedded += bytes.length;
    if (!report.images.some((image) => image.trial === trial && image.path === path)) {
      report.images.push({ trial, path, kind, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex").slice(0, 16), note: "" });
    }
    return { ...shown, image: dataUri(type, bytes) };
  }
  const raw = bytes.toString("utf8");
  if (kind === "frame") {
    const frame = maskFrame(raw);
    if (frame.residue) return { ...shown, problem: "refused: a value in this frame survived masking", masked: true };
    const svg = Buffer.from(frameSvg(frame.lines));
    if (overCap(report, svg.length)) return { ...shown, problem: overCap(report, svg.length), masked: frame.changed };
    report.embedded += svg.length;
    return { ...shown, image: dataUri("image/svg+xml", svg), masked: frame.changed };
  }
  return { ...shown, text: kind === "db" ? pretty(raw) : raw, truncated };
}

function flagRecordings(images, trial, parts) {
  if (!parts.some((part) => part.evidence.some((item) => item.masked))) return;
  for (const image of images) if (image.trial === trial && !image.leftOut && image.path.toLowerCase().endsWith(".gif")) image.note = FLAGGED;
}

function kindOf(path) {
  const type = IMAGES[extname(String(path)).toLowerCase()];
  if (type) return "screenshot";
  if (String(path).startsWith("frames/")) return "frame";
  return String(path).endsWith(".json") ? "db" : "log";
}

function natural(left, right) {
  return left.localeCompare(right, "en", { numeric: true });
}

function span(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "";
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return hours ? `${hours} h ${minutes} min` : `${minutes} min ${Math.round(seconds % 60)} s`;
}

function byKind(kinds) {
  const counts = new Map();
  for (const kind of kinds) counts.set(kind, (counts.get(kind) ?? 0) + 1);
  return [...counts].sort((left, right) => right[1] - left[1] || natural(left[0], right[0])).map(([kind, count]) => `${kind} ×${count}`);
}

function usage(tokens) {
  if (!tokens || typeof tokens !== "object") return [];
  if (Number.isFinite(tokens.before) && Number.isFinite(tokens.after)) return [`before ${tokens.before}`, `after ${tokens.after}`, `used ${tokens.after - tokens.before}`];
  return Object.entries(tokens).map(([key, value]) => `${key} ${JSON.stringify(value)}`);
}

/**
 * Reads one trial run folder written to the DEC-112 evidence contract: manifest.json,
 * steps.jsonl and checks/<id>.json with their evidence files.
 * Input: the folder path. Output: the report model; evidence outside the folder is refused, not read.
 */
export async function readTrial(folder, options = {}) {
  KNOWN = knownValues(options);
  const root = await realpath(folder);
  const manifest = await readJson(root, "manifest.json", "report_manifest_invalid");
  if (!manifest || typeof manifest !== "object" || !Number.isSafeInteger(manifest.trial)) fail("report_manifest_invalid: trial");
  const report = { images: [], embedded: 0 };
  const steps = [];
  const stepsText = await optionalText(root, "steps.jsonl", TEXT_BYTES * 16);
  for (const [index, line] of stepsText.split("\n").filter((entry) => entry.trim()).entries()) {
    let step;
    try {
      step = JSON.parse(line);
    } catch {
      fail(`report_steps_invalid: line ${index + 1}`);
    }
    const items = [];
    for (const path of Array.isArray(step.evidence) ? step.evidence : []) items.push(await evidence(root, { kind: kindOf(path), path, caption: path }, report, manifest.trial));
    steps.push({ n: step.n ?? index + 1, at: step.at, surface: step.surface, title: step.title, why: step.why, ok: step.ok, detail: step.detail, evidence: items });
  }
  let names = [];
  try {
    names = (await readdir(join(root, "checks"))).filter((name) => name.endsWith(".json"));
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const checks = [];
  for (const name of names.sort(natural)) {
    const check = await readJson(root, `checks/${name}`, "report_check_invalid");
    const items = [];
    for (const item of Array.isArray(check.evidence) ? check.evidence : []) items.push(await evidence(root, item, report, manifest.trial));
    const valid = RESULTS.includes(check.result);
    checks.push({
      id: String(check.id ?? name.replace(/\.json$/, "")),
      title: check.title, proves: check.proves, why: check.why, how: check.how,
      result: valid ? check.result : "UNKNOWN",
      detail: valid ? check.detail : `The result field was ${JSON.stringify(check.result)}. ${check.detail ?? ""}`,
      evidence: items,
    });
  }
  checks.sort((left, right) => natural(left.id, right.id));
  flagRecordings(report.images, manifest.trial, [...steps, ...checks]);
  const started = Date.parse(manifest.started);
  const ended = Date.parse(manifest.ended);
  return {
    kind: "trial",
    title: `Trial ${manifest.trial} report`,
    note: "",
    known: KNOWN,
    trials: [{
      trial: manifest.trial,
      build: manifest.build ?? "",
      appVersion: manifest.app_version ?? "",
      date: typeof manifest.started === "string" ? manifest.started.slice(0, 10) : "",
      started: manifest.started ?? "",
      ended: manifest.ended ?? "",
      duration: span((ended - started) / 1000),
      project: manifest.project ?? "",
      accounts: byKind(Array.isArray(manifest.accounts) ? manifest.accounts.map(String) : []),
      tokens: usage(manifest.tokens),
      why: [`Trial ${manifest.trial} ran the owner's scenario on the installed build and audited the result. Each step says why it was taken; each check says what it proves and which requirement it answers.`],
      plan: [],
      steps,
      checks,
      record: [],
    }],
    images: report.images,
  };
}

function parsePlan(raw) {
  const sections = [{ heading: "", items: [] }];
  let title = "";
  for (const line of raw.split("\n")) {
    if (line.startsWith("# ")) {
      title ||= line.slice(2).trim();
      continue;
    }
    if (line.startsWith("## ")) {
      sections.push({ heading: line.slice(3).trim(), items: [] });
      continue;
    }
    if (!line.trim()) continue;
    const numbered = line.match(/^(\d+)\.\s+(.*)$/);
    const bullet = line.match(/^\s*-\s+(.*)$/);
    sections.at(-1).items.push(numbered ? { number: numbered[1], text: numbered[2] } : { text: bullet ? bullet[1] : line.trim() });
  }
  return { title, intro: sections[0].items.map((item) => item.text), sections: sections.slice(1) };
}

function parseAudit(raw) {
  const checks = [];
  let summary = "";
  for (const line of raw.split("\n")) {
    const head = line.match(/^(PASS|FAIL|UNKNOWN) (\S+) ([^:]+): ?(.*)$/);
    if (head) {
      checks.push({ result: head[1], id: head[2], title: head[3], detail: head[4], lines: [line] });
    } else if (line.startsWith("SUMMARY ")) {
      summary = line;
    } else if (line.trim() && checks.length) {
      checks.at(-1).lines.push(line);
    }
  }
  return { checks, summary };
}

function parseTimeline(raw) {
  return raw.split("\n").map((line) => line.match(/^(\d{2}):(\d{2}):(\d{2})\s*(.*)$/)).filter(Boolean).map((match) => ({
    at: `${match[1]}:${match[2]}:${match[3]}`,
    seconds: Number(match[1]) * 3600 + Number(match[2]) * 60 + Number(match[3]),
    text: match[4].trim(),
  }));
}

function tokenTotals(raw) {
  const totals = { input: 0, output: 0, cache_read: 0, cache_create: 0, turns: 0 };
  let agents = 0;
  for (const line of raw.split("\n")) {
    if (!/^(?:agent )?\d+ \{/.test(line)) continue;
    agents += 1;
    for (const [, key, value] of line.matchAll(/'(\w+)': (\d+)/g)) if (key in totals) totals[key] += Number(value);
  }
  if (!agents) return [];
  return [`input ${totals.input}`, `output ${totals.output}`, `cache read ${totals.cache_read}`, `cache create ${totals.cache_create}`, `turns ${totals.turns} (${agents} agent${agents === 1 ? "" : "s"})`];
}

async function retroTrial(root, spec, report) {
  const auditText = await optionalText(root, spec.audit);
  if (!auditText) fail(`report_retro_missing: ${spec.audit}`);
  const plan = parsePlan(await optionalText(root, spec.plan));
  const audit = parseAudit(auditText);
  const planned = new Map(plan.sections.filter((section) => /^checks/i.test(section.heading)).flatMap((section) => section.items.filter((item) => item.number).map((item) => [item.number, item.text])));
  const after = plan.sections.find((section) => /^after/i.test(section.heading))?.items.map((item) => item.text) ?? [];
  const measured = after.find((line) => line.includes("trial-audit.sh")) ?? `the saved ${spec.audit}`;
  const transcripts = after.find((line) => /transcript/i.test(line)) ?? measured;
  const checks = audit.checks.map((check) => {
    const wording = planned.get(check.id);
    const prior = wording?.match(/\[([^\]]+)\]\s*$/)?.[1];
    return {
      id: check.id,
      title: check.title,
      result: check.result,
      detail: check.detail,
      proves: wording ? wording.replace(/\s*\[[^\]]+\]\s*$/, "") : `Transcript and engine audit: ${check.title}.`,
      why: [plan.title || `Trial ${spec.trial}`, prior ? `first trial: ${prior}` : ""].filter(Boolean).join("; "),
      how: `Read after the run: ${wording ? measured : transcripts}`,
      evidence: [{ kind: "log", path: spec.audit, caption: `${spec.audit}, check ${check.id}`, text: check.lines.join("\n") }],
    };
  });
  const times = parseTimeline(await optionalText(root, spec.timeline));
  const end = times.find((time) => /stop all/i.test(time.text)) ?? times.at(-1);
  const seconds = times.length ? (end.seconds - times[0].seconds + 86400) % 86400 : NaN;
  const limits = await optionalText(root, spec.limits);
  const build = plan.intro.find((line) => /^Build /.test(line)) ?? "";
  const version = build.match(/^Build (\d+(?:\.\d+)*)/)?.[1] ?? "";
  const sha = build.match(/\b[0-9a-f]{8}\b/)?.[0];
  const frames = [];
  let names = [];
  try {
    names = (await readdir(join(root, spec.frames))).filter((name) => name.endsWith(".txt")).sort(natural);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  for (const name of names) frames.push(await evidence(root, { kind: "frame", path: `${spec.frames}/${name}`, caption: `TUI frame ${name}` }, report, spec.trial));
  const record = [...frames];
  const gif = await place(root, spec.gif);
  if (!frames.length) record.push(await evidence(root, { kind: "gif", path: spec.gif, caption: `${spec.gif}, the TUI record` }, report, spec.trial));
  else if (!gif.problem) report.images.push({ trial: spec.trial, path: spec.gif, kind: "gif", bytes: gif.bytes, sha256: "", note: LEFT_OUT, leftOut: true });
  flagRecordings(report.images, spec.trial, [{ evidence: record }]);
  return {
    trial: spec.trial,
    build: sha ? `${version} (${sha})` : version,
    appVersion: "",
    date: limits.match(/^at (\d{4}-\d{2}-\d{2})/m)?.[1] ?? "",
    started: times[0]?.at ?? "",
    ended: end?.at ?? "",
    duration: span(seconds),
    project: audit.summary.match(/project=(\S+)/)?.[1] ?? "",
    accounts: byKind([...limits.matchAll(/'([a-z]+)-account-[A-Za-z0-9]+'/g)].map((match) => ACCOUNT_KINDS[match[1]] ?? match[1])),
    tokens: tokenTotals(await optionalText(root, spec.tokens)),
    why: [plan.title, ...plan.intro].filter(Boolean),
    plan: plan.sections.filter((section) => !/^checks/i.test(section.heading)),
    steps: times.filter((time) => time.text).map((time, index) => ({ n: index + 1, at: time.at, surface: "tui", title: time.text, why: "", ok: null, detail: "", evidence: [] })),
    checks,
    record,
  };
}

/**
 * Reads the saved evidence of trials 1 and 2 in leader/trial: audit.txt and audit-2.txt, the trial plans,
 * timelines, token sums, limits (account kinds only), TUI frames and GIFs.
 * Input: the leader/trial folder. Output: one report model holding both trials.
 */
export async function readRetro(folder, options = {}) {
  KNOWN = knownValues(options);
  const root = await realpath(folder);
  const report = { images: [], embedded: 0 };
  const trials = [];
  for (const spec of RETRO) trials.push(await retroTrial(root, spec, report));
  return {
    kind: "trial",
    title: "Trials 1 and 2, rebuilt from the saved evidence",
    known: KNOWN,
    note: "Results come from the saved audits, what each check proves and why from the trial plans, and the frames are the recorded TUI text drawn again after masking.",
    trials,
    images: report.images,
  };
}

function parseTests(raw) {
  const lines = raw.split("\n");
  const start = lines.findIndex((line) => /cat <<'EOF'\s*$/.test(line));
  const stop = lines.indexOf("EOF", start + 1);
  if (start < 0 || stop < 0) fail("report_live_tests_missing");
  return lines.slice(start + 1, stop).filter((line) => line.trim()).map((line) => {
    const [group, target, name, seconds, needs, ...extra] = line.split("|");
    if (extra.length || needs === undefined || !/^[a-z]+$/.test(group) || !/^[A-Za-z0-9_]+$/.test(target) || !/^[A-Za-z0-9_:]+$/.test(name) || !/^\d+$/.test(seconds)) {
      fail(`report_live_tests_invalid: ${line}`);
    }
    return { group, target, name, needs };
  });
}

function parseSummary(raw) {
  const results = new Map();
  const groups = new Map();
  for (const line of raw.split("\n")) {
    let match;
    if ((match = line.match(/^== (\w+): (.*)$/))) groups.set(match[1], match[2]);
    else if ((match = line.match(/^PASS (\S+) (\d+)s$/))) results.set(match[1], { result: "PASS", seconds: Number(match[2]) });
    else if ((match = line.match(/^FAIL (\S+) (\d+)s exit (-?\d+): /))) results.set(match[1], { result: "FAIL", seconds: Number(match[2]), exit: Number(match[3]) });
    else if ((match = line.match(/^BLOCKED (\S+)(?: (\d+)s)?: (.*)$/))) {
      results.set(match[1], { result: "BLOCKED", seconds: match[2] === undefined ? undefined : Number(match[2]), reason: match[3].replace(/^BLOCKED: /, "") });
    }
  }
  return { results, groups };
}

async function logFolder(root, path) {
  const full = join(root, path);
  const info = await lstat(full).catch((error) => (error.code === "ENOENT" || error.code === "ENOTDIR" ? null : Promise.reject(error)));
  if (info === null) return { problem: `missing: ${path}` };
  if (info.isSymbolicLink()) return { problem: "refused: a link" };
  if (!info.isDirectory()) return { problem: "refused: not a folder" };
  const real = await realpath(full);
  const inner = relative(root, real);
  if (inner === "" || inner.startsWith("..") || isAbsolute(inner)) return { problem: "refused: outside the run folder" };
  return { real };
}

/**
 * Lists the summaries of a live-pass folder in merge order: the runs runs.json names, or summary.txt alone.
 * Input: the real folder path. Output: each run's file, commit (empty for summary.txt), source, log folder
 * (empty for the folder itself), text and time. Throws report_live_runs_invalid for a malformed runs.json,
 * a listed summary that is not a plain file here, or a log folder that is not a plain folder here or is another run's.
 */
async function liveRuns(root) {
  if ((await place(root, "runs.json")).problem?.startsWith("missing:")) {
    const summary = await optionalText(root, "summary.txt");
    if (!summary) fail("report_live_summary_missing");
    return [{ file: "summary.txt", commit: "", source: "", logs: "", text: summary, written: (await stat(join(root, "summary.txt"))).mtime }];
  }
  const listed = await readJson(root, "runs.json", "report_live_runs_invalid");
  if (!Array.isArray(listed) || listed.length === 0 || listed.length > RUNS_MAX) fail(`report_live_runs_invalid: a list of 1 to ${RUNS_MAX} runs`);
  const runs = [];
  const owners = new Map();
  for (const run of listed) {
    const { file, commit, source, logs } = run ?? {};
    if (typeof file !== "string" || !RUN_FILE.test(file) || typeof commit !== "string" || !RUN_COMMIT.test(commit) || typeof source !== "string" || source.length > RUN_SOURCE_MAX) {
      fail("report_live_runs_invalid: each run names a summary*.txt file here, its commit hash and a short source");
    }
    if (typeof logs !== "string" || logs.length > RUN_LOGS_MAX || !RUN_LOGS.test(logs)) fail(`report_live_runs_invalid: ${file} needs logs, the plain folder here that holds its run's logs`);
    const where = await place(root, file);
    if (where.problem) fail(`report_live_runs_invalid: ${file} ${where.problem}`);
    const logsAt = await logFolder(root, logs);
    if (logsAt.problem) fail(`report_live_runs_invalid: ${file} logs ${logsAt.problem}`);
    if (owners.has(logsAt.real)) fail(`report_live_runs_invalid: ${file} logs ${logs} is already the log folder of ${owners.get(logsAt.real)}`);
    owners.set(logsAt.real, file);
    runs.push({ file, commit, source, logs, text: (await readBounded(where.full, TEXT_BYTES * 4)).bytes.toString("utf8"), written: (await stat(where.full)).mtime });
  }
  return runs;
}

async function testSources(repo) {
  const sources = new Map();
  for (const folder of ["crates/crazyagent/src", "crates/crazyagent/tests"]) {
    let names = [];
    try {
      names = await readdir(join(repo, folder), { recursive: true });
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    for (const name of names.filter((entry) => entry.endsWith(".rs"))) {
      const path = `${folder}/${name}`;
      const info = await lstat(join(repo, path));
      if (info.isFile()) sources.set(path, (await readBounded(join(repo, path), SOURCE_BYTES)).bytes.toString("utf8"));
    }
  }
  return sources;
}

function docBefore(source, index) {
  const lines = source.slice(0, index).split("\n");
  lines.pop();
  const doc = [];
  for (let at = lines.length - 1; at >= 0; at -= 1) {
    const line = lines[at].trim();
    if (line.startsWith("#[")) continue;
    const match = line.match(/^\/\/\/\s?(.*)$/);
    if (!match) break;
    doc.unshift(match[1]);
  }
  return doc.join(" ").trim();
}

function docFor(sources, target, name) {
  const parts = name.split("::");
  const leaf = parts.at(-1);
  const pattern = new RegExp(`^[ \\t]*(?:pub(?:\\([^)]*\\))?\\s+)?(?:async\\s+)?fn\\s+${leaf}\\s*[(<]`, "m");
  let best = null;
  for (const [path, source] of sources) {
    const owned = target === "lib" ? path.startsWith("crates/crazyagent/src/") : path === `crates/crazyagent/tests/${target}.rs` || path.startsWith(`crates/crazyagent/tests/${target}/`);
    if (!owned) continue;
    const match = pattern.exec(source);
    if (!match) continue;
    const doc = docBefore(source, match.index);
    const score = parts.slice(0, -1).filter((part) => part !== "tests" && path.includes(part)).length;
    if (doc && (!best || score > best.score)) best = { doc, score };
  }
  return best?.doc ?? "";
}

function sentence(name) {
  const words = name.split("::").at(-1).replace(/_/g, " ").trim();
  return `${words.charAt(0).toUpperCase()}${words.slice(1)}.`;
}

function passProblem(name, { bytes, truncated }) {
  if (truncated) return "the log is too large to read whole";
  const lines = bytes.toString("utf8").split("\n");
  if (lines.includes(`test ${name} ... ok`) && lines.some((line) => line.startsWith("test result: ok. 1 passed"))) return null;
  return `the log does not show "test ${name} ... ok" and "test result: ok. 1 passed"`;
}

function withoutProcessLists(lines) {
  const kept = [];
  let left = 0;
  for (const line of [...lines, null]) {
    if (line !== null && PS_LINES.some((pattern) => pattern.test(line))) {
      left += 1;
      continue;
    }
    if (left) kept.push(`[process list left out: ${left} line${left === 1 ? "" : "s"}]`);
    left = 0;
    if (line !== null) kept.push(line);
  }
  return kept;
}

function excerpt(raw) {
  const lines = withoutProcessLists(raw.replace(/\s+$/, "").split("\n"));
  const at = lines.findIndex((line) => /panicked at|^BLOCKED|^error(\[|:)/.test(line));
  const from = at < 0 ? Math.max(0, lines.length - EXCERPT_LINES) : Math.max(0, at - 2);
  return lines.slice(from, from + EXCERPT_LINES).join("\n");
}

/**
 * Reads a live-pass folder written by leader/live-pass-18.1x.sh: summary.txt with one log per test beside it, or
 * the summaries runs.json lists, each with its commit and its own log folder, later runs replacing earlier results.
 * Input: the folder, the script that lists the tests and the checkout holding their source.
 * Output: one row per listed test with its run and excerpt; a PASS whose own log does not show it passing reads UNPROVEN.
 */
export async function readLivePass(folder, { script, repo = REPO, ...options } = {}) {
  KNOWN = knownValues(options);
  const root = await realpath(folder);
  const runs = await liveRuns(root);
  if (typeof script !== "string" || !script) fail("report_live_script_required");
  const listed = parseTests(await readFile(script, "utf8"));
  const results = new Map();
  const groups = new Map();
  for (const run of runs) {
    const parsed = parseSummary(run.text);
    for (const [group, about] of parsed.groups) groups.set(group, about);
    for (const [name, outcome] of parsed.results) results.set(name, { ...outcome, run: run.commit, logs: run.logs });
  }
  const sources = await testSources(repo);
  const known = new Set(listed.map((entry) => entry.name));
  const all = [...listed, ...[...results.keys()].filter((name) => !known.has(name)).map((name) => ({ group: "unlisted", target: "lib", name }))];
  const rows = [];
  for (const [index, entry] of all.entries()) {
    const outcome = results.get(entry.name) ?? { result: "NOT RUN" };
    const doc = docFor(sources, entry.target, entry.name);
    const log = outcome.logs ? `${outcome.logs}/${entry.name.replaceAll("::", ".")}.log` : `${entry.name.replaceAll("::", ".")}.log`;
    const where = outcome.result === "NOT RUN" ? null : await place(root, log);
    let result = outcome.result;
    let detail = "";
    if (outcome.result === "BLOCKED") detail = outcome.seconds !== undefined && where.problem ? `${outcome.reason} (log ${where.problem})` : outcome.reason;
    if (outcome.result === "FAIL") detail = where.problem ? where.problem : excerpt((await readBounded(where.full, LOG_BYTES)).bytes.toString("utf8"));
    if (outcome.result === "PASS") {
      const problem = where.problem ?? passProblem(entry.name, await readBounded(where.full, LOG_BYTES));
      if (problem) [result, detail] = ["UNPROVEN", problem];
    }
    rows.push({ n: index + 1, group: entry.group, name: entry.name, proves: doc || sentence(entry.name), fromName: !doc, result, run: outcome.run ?? "", seconds: outcome.seconds, detail });
  }
  const count = (result) => rows.filter((row) => row.result === result).length;
  const written = new Date(Math.max(...runs.map((run) => run.written.getTime())));
  return {
    kind: "live",
    known: KNOWN,
    title: `Live pass ${basename(root)}`,
    folder: root,
    date: written.toISOString().slice(0, 10),
    groups: [...groups],
    runs: runs.filter((run) => run.commit).map(({ file, commit, source }) => ({ file, commit, source })),
    rows,
    totals: `${count("PASS")} passed, ${count("FAIL")} failed, ${count("BLOCKED")} blocked, ${count("UNPROVEN") ? `${count("UNPROVEN")} unproven, ` : ""}${count("NOT RUN")} not run`,
    time: span(rows.reduce((total, row) => total + (row.seconds ?? 0), 0)),
  };
}

function badge(result) {
  return `<span class="result ${result.toLowerCase().replace(/\s+/g, "-")}">${text(result)}</span>`;
}

function facts(rows) {
  return `<dl>${rows.filter(([, value]) => value !== "" && value != null).map(([label, value]) => `<dt>${text(label)}</dt><dd>${text(value)}</dd>`).join("")}</dl>`;
}

function evidenceItem(entry) {
  const caption = `<figcaption>${text(entry.caption)} <span class="path">${text(entry.kind)} · ${text(entry.path)}</span></figcaption>`;
  if (entry.problem) return `<figure>${caption}<p class="problem">${text(entry.problem)}</p></figure>`;
  if (entry.image) return `<figure>${caption}<img alt="${text(entry.caption)}" src="${entry.image}"></figure>`;
  return `<figure>${caption}<pre>${text(entry.text)}</pre>${entry.truncated ? `<p class="note">Cut at ${TEXT_BYTES} bytes.</p>` : ""}</figure>`;
}

function tally(checks) {
  return RESULTS.map((result) => `${result} ${checks.filter((check) => check.result === result).length}`).join(", ");
}

function cover(trial) {
  return `<h2>Trial ${text(trial.trial)}</h2>${facts([
    ["Build", trial.build], ["App version", trial.appVersion], ["Date", trial.date], ["Project", trial.project],
    ["Accounts by kind", trial.accounts.join(", ")], ["Duration", trial.duration], ["Tokens", trial.tokens.join(", ")],
    ["Checks", tally(trial.checks)],
  ])}`;
}

function planSection(section) {
  if (/^master message/i.test(section.heading)) return `<h4>${text(section.heading)}</h4><blockquote>${text(section.items.map((item) => item.text).join("\n"))}</blockquote>`;
  const tag = section.items.every((item) => item.number) ? "ol" : "ul";
  return `<h4>${text(section.heading)}</h4><${tag}>${section.items.map((item) => `<li>${text(item.text)}</li>`).join("")}</${tag}>`;
}

function trialSection(trial, many) {
  const prefix = many ? `trial${trial.trial}-` : "";
  const parts = [`<section class="trial"><h2>Trial ${text(trial.trial)}</h2><h3>What we did and why</h3>`];
  parts.push(...trial.why.map((line) => `<p>${text(line)}</p>`), ...trial.plan.map(planSection));
  if (trial.steps.length) {
    const judged = trial.steps.some((step) => step.why || typeof step.ok === "boolean");
    parts.push(`<h4>Steps</h4><table class="steps"><thead><tr><th>#</th><th>At</th><th>Surface</th><th>Step</th>${judged ? "<th>Why</th><th>OK</th>" : ""}</tr></thead><tbody>`);
    for (const step of trial.steps) {
      const ok = step.ok === true ? badge("PASS") : step.ok === false ? badge(/^BLOCKED: /.test(step.detail ?? "") ? "BLOCKED" : "FAIL") : "";
      const more = judged ? `<td>${text(step.why ?? "")}</td><td>${ok}</td>` : "";
      parts.push(`<tr><td>${text(step.n)}</td><td>${text(step.at ?? "")}</td><td>${text(step.surface ?? "")}</td><td>${text(step.title ?? "")}${step.detail ? `<div class="note">${text(step.detail)}</div>` : ""}</td>${more}</tr>`);
    }
    parts.push("</tbody></table>");
    for (const step of trial.steps.filter((entry) => entry.evidence.length)) parts.push(`<h4>Step ${text(step.n)}: ${text(step.title ?? "")}</h4>${step.evidence.map(evidenceItem).join("")}`);
  }
  parts.push(`<h3>Summary</h3><table class="summary"><thead><tr><th>Check</th><th>Title</th><th>Result</th></tr></thead><tbody>`);
  parts.push(...trial.checks.map((check) => `<tr><td>${text(check.id)}</td><td>${text(check.title ?? "")}</td><td>${badge(check.result)}</td></tr>`), "</tbody></table><h3>Checks</h3>");
  for (const check of trial.checks) {
    parts.push(`<article class="check" id="${prefix}${text(check.id)}"><h4>${text(check.id)} ${text(check.title ?? "")} ${badge(check.result)}</h4>`);
    parts.push(facts([["Proves", check.proves ?? ""], ["Why", check.why ?? ""], ["How", check.how ?? ""], ["Result", check.result], ["Detail", check.detail ?? ""]]).replace(/<dt>(Proves|Why|How|Detail)<\/dt><dd><\/dd>/g, ""));
    parts.push(check.evidence.length ? check.evidence.map(evidenceItem).join("") : `<p class="problem">${text(check.id)} has no evidence item.</p>`, "</article>");
  }
  if (trial.record.length) parts.push(`<h3>Recording</h3>${trial.record.map(evidenceItem).join("")}`);
  parts.push("</section>");
  return parts.join("\n");
}

function imageItem(image) {
  return `<li>Trial ${text(image.trial)} ${text(image.kind)}: <code>${text(image.path)}</code>${image.note ? ` <span class="note">${text(image.note)}</span>` : ""}</li>`;
}

function sharing(images, known) {
  const shown = images.filter((image) => !image.leftOut);
  const left = images.filter((image) => image.leftOut);
  const count = `${shown.length} image${shown.length === 1 ? "" : "s"}`;
  return `<section class="sharing"><h2>Before sharing</h2><p>${text(known.note)}</p><p><b>Not masked: ${count}.</b> Screenshots, image frames, GIFs and SVG files are not masked: they are pictures, and the report cannot mask what they show. Look at each one before this report is shared. Logs, rows, plans and the text TUI frames are masked, account labels and home paths included.</p>${shown.length ? `<ul>${shown.map(imageItem).join("")}</ul>` : ""}${left.length ? `<p>Left out of this report:</p><ul>${left.map(imageItem).join("")}</ul>` : ""}</section>`;
}

function imagesSection(all) {
  const images = all.filter((image) => !image.leftOut);
  const left = all.filter((image) => image.leftOut);
  const rows = images.map((image) => `<tr><td>${text(image.trial)}</td><td><code>${text(image.path)}</code></td><td>${text(image.bytes)}</td><td><code>${text(image.sha256)}</code></td><td>${text(image.note)}</td></tr>`);
  return `<section class="images"><h2>Images embedded without masking</h2><p>Screenshots and GIFs are pictures, so the report cannot mask them. Look at each one below before this report is shared. Text TUI frames are drawn from masked text and are not listed. SVG files are listed: their text passed the mask check, but an SVG can carry pictures.</p>${rows.length ? `<table><thead><tr><th>Trial</th><th>Image</th><th>Bytes</th><th>SHA-256</th><th>Note</th></tr></thead><tbody>${rows.join("")}</tbody></table>` : "<p>None.</p>"}${left.length ? `<h3>Left out</h3><ul>${left.map(imageItem).join("")}</ul>` : ""}</section>`;
}

function page(title, body) {
  return `<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'"><title>${text(title)}</title><style>${CSS}</style></head><body>\n${body}\n</body></html>\n`;
}

function renderTrials(model) {
  const many = model.trials.length > 1;
  const head = `<header class="cover"><h1>${text(model.title)}</h1><p class="generated">Generated ${text(new Date().toISOString())} by scripts/trial/report.mjs.</p>${model.note ? `<p>${text(model.note)}</p>` : ""}${sharing(model.images, model.known)}${model.trials.map(cover).join("")}</header>`;
  return page(model.title, [head, ...model.trials.map((trial) => trialSection(trial, many)), imagesSection(model.images)].join("\n"));
}

function runsList(runs) {
  if (!runs?.length) return "";
  return `<h3>Runs</h3><p>The summaries merged into this report, in order. A later run replaces an earlier result for the same test; each row names the commit of the run its result came from.</p><ul>${runs.map((run) => `<li><code>${text(run.file)}</code> at <code>${text(run.commit)}</code>: ${text(run.source)}</li>`).join("")}</ul>`;
}

function renderLive(model) {
  const head = `<header class="cover"><h1>${text(model.title)}</h1><p class="generated">Generated ${text(new Date().toISOString())} by scripts/trial/report.mjs from ${text(model.folder)}.</p><p>${text(model.known.note)}</p>${facts([["Date", model.date], ["Tests", model.rows.length], ["Results", model.totals], ["Test time", model.time]])}<h3>Groups</h3><ul>${model.groups.map(([group, about]) => `<li><b>${text(group)}</b>: ${text(about)}</li>`).join("")}</ul>${runsList(model.runs)}</header>`;
  const rows = model.rows.map((row) => {
    const proves = `${text(row.proves)}${row.fromName ? ' <span class="source">from the test name</span>' : ""}`;
    const run = row.run ? ` <span class="source">${text(row.run)}</span>` : "";
    const line = `<tr class="test"><td>${row.n}</td><td>${text(row.group)}</td><td><code>${text(row.name)}</code></td><td>${proves}</td><td>${badge(row.result)}${run}</td><td>${row.seconds === undefined ? "" : `${row.seconds} s`}</td></tr>`;
    return row.detail ? `${line}\n<tr class="excerpt"><td colspan="6"><pre>${text(row.detail)}</pre></td></tr>` : line;
  });
  const table = `<section class="live"><h2>Tests</h2><table><thead><tr><th>#</th><th>Group</th><th>Test</th><th>What it proves</th><th>Result</th><th>Duration</th></tr></thead><tbody>\n${rows.join("\n")}\n</tbody></table></section>`;
  return page(model.title, `${head}\n${table}`);
}

/**
 * Renders a report model from readTrial, readRetro or readLivePass as one self-contained HTML page.
 * Every text that enters the page is masked, then escaped; images are inline data URIs.
 * Input: the model. Output: the HTML string.
 */
export function renderReport(model) {
  if (!Array.isArray(model?.known?.rules) || typeof model.known.note !== "string") fail("report_model_invalid");
  KNOWN = model.known;
  if (model.kind === "trial") return renderTrials(model);
  if (model?.kind === "live") return renderLive(model);
  return fail("report_model_invalid");
}

/**
 * Prints an HTML file to PDF with Google Chrome headless under umask 077, in a throwaway profile with a mock
 * keychain, every request sent to a closed proxy and every host name unresolvable.
 * Input: the HTML path, the PDF path and optionally the Chrome executable (CRAZYAGENT_CHROME or the macOS app).
 * Output: the PDF path, mode 0600, moved into place only once complete. Throws a BLOCKED error when Chrome is missing.
 */
export async function printPdf(htmlPath, pdfPath, { chrome = process.env.CRAZYAGENT_CHROME || CHROME, timeout = 120_000 } = {}) {
  try {
    await stat(chrome);
  } catch {
    fail(`BLOCKED: Google Chrome is not at ${chrome}; set CRAZYAGENT_CHROME to its executable`);
  }
  const profile = await mkdtemp(join(tmpdir(), "trial-report-chrome-"));
  const staging = await mkdtemp(join(dirname(pdfPath), ".report-pdf-"));
  try {
    await rm(pdfPath, { force: true });
    const staged = join(staging, "report.pdf");
    const args = [
      "--headless", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--disable-extensions",
      "--disable-background-networking", "--disable-component-update", "--disable-sync", "--use-mock-keychain", "--password-store=basic",
      "--proxy-server=127.0.0.1:9", "--proxy-bypass-list=<-loopback>", "--host-resolver-rules=MAP * ~NOTFOUND",
      `--user-data-dir=${profile}`, "--no-pdf-header-footer", `--print-to-pdf=${staged}`, pathToFileURL(htmlPath).href,
    ];
    await new Promise((done, failed) => {
      execFile("/bin/sh", ["-c", 'umask 077 && exec "$0" "$@"', chrome, ...args], { timeout, killSignal: "SIGKILL", maxBuffer: 4 * 1024 * 1024 }, (error) => (error ? failed(new Error(`report_pdf_failed: ${error.message.split("\n")[0]}`)) : done()));
    });
    const where = await lstat(staged).catch(() => fail("report_pdf_missing"));
    if (!where.isFile()) fail("report_pdf_missing");
    if (where.mode & 0o077) fail("report_pdf_mode: Chrome wrote the PDF readable by others");
    const { bytes } = await readBounded(staged, 5);
    if (bytes.toString("latin1") !== "%PDF-") fail("report_pdf_invalid");
    await chmod(staged, 0o600);
    await rename(staged, pdfPath);
    return pdfPath;
  } finally {
    await rm(profile, { recursive: true, force: true });
    await rm(staging, { recursive: true, force: true });
  }
}

/**
 * Runs the report command: reads the run folder, writes report.html (masked, checked for residue)
 * and prints report.pdf next to it, or into --out. Input: the argument list, see USAGE.
 * Output: the PDF path.
 */
export async function main(argv) {
  const [mode, folder, ...rest] = argv;
  const options = {};
  for (let at = 0; at < rest.length; at += 2) {
    if (!["--script", "--repo", "--engine", "--out", "--chrome"].includes(rest[at]) || rest[at + 1] === undefined) fail(`report_usage: ${USAGE}`);
    options[rest[at].slice(2)] = rest[at + 1];
  }
  if (!folder || !["trial", "live", "retro"].includes(mode)) fail(`report_usage: ${USAGE}`);
  const engine = options.engine ?? ENGINE;
  const database = await lstat(join(engine, "engine.db")).catch(() => null);
  if (!database?.isFile()) fail(`BLOCKED: no engine database at ${join(engine, "engine.db")}; pass --engine <engine root> so account labels are masked by value`);
  const model = mode === "trial" ? await readTrial(folder, { engine }) : mode === "live" ? await readLivePass(folder, { ...options, engine }) : await readRetro(folder, { engine });
  const out = await realpath(options.out ?? folder);
  const html = assertMasked(renderReport(model));
  const htmlPath = join(out, "report.html");
  await rm(htmlPath, { force: true });
  await writeFile(htmlPath, html, { mode: 0o600, flag: "wx" });
  return printPdf(htmlPath, join(out, "report.pdf"), { chrome: options.chrome });
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main(process.argv.slice(2)).then((pdf) => console.log(pdf), (error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
