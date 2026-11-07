import assert from "node:assert/strict";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const LIB = fileURLToPath(new URL("../../src-tauri/src/lib.rs", import.meta.url));
export const ROUTES = fileURLToPath(new URL("../../src-tauri/e2e_bridge/routes.rs", import.meta.url));
export const BRIDGE = fileURLToPath(new URL("../../src-tauri/src/bridge", import.meta.url));

const NATIVE_ONLY = /\bbridge::|\bupdates::|\bterminals\b|\bapp\b|NativeBridge::|Arc::|default_engine\(|SOCKET_OVERRIDE|state\.0/;
const NATIVE_PARAM = /State<|AppHandle|Channel<|WebviewWindow/;
const QUEUE_CALL = /^state\.0\.\w+\([^;]*\)\s*\.await$/;
const IMPL_TYPES = ["NativeBridge", "BridgeQueue"];
const BRIDGE_FILES_MAX = 256;

/** Splits text at the commas that sit outside every bracket.
 * Input: a parameter list or brace list. Output: the trimmed, non-empty parts. */
function splitTop(text) {
  const parts = [];
  let depth = 0;
  let start = 0;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if ("<([{".includes(char)) depth += 1;
    else if (">)]}".includes(char)) depth -= 1;
    else if (char === "," && depth === 0) {
      parts.push(text.slice(start, index));
      start = index + 1;
    }
  }
  parts.push(text.slice(start));
  return parts.map((part) => part.trim()).filter(Boolean);
}

/** Splits Rust text into tokens without layout: words, string literals, lifetimes, `::`, `..`,
 * `->` and single marks, `///` lines dropped and a comma before a closing bracket left out, so
 * two rustfmt layouts of the same code give the same list. Input: Rust text. Output: tokens. */
function tokens(text) {
  const words = text.replace(/^\s*\/\/\/.*$/gm, "").match(/[A-Za-z_]\w*|\d+|"(?:[^"\\]|\\.)*"|'[a-z_]\w*|::|\.\.|->|\S/g) ?? [];
  return words.filter((word, index) => !(word === "," && /^[)}\]>]$/.test(words[index + 1] ?? "")));
}

/** The one `CommandArg` impl the generator follows, the way lib.rs writes it for an argument
 * whose fields Tauri reads one by one: a fixed head, the command item taken apart, one read
 * closure that hands each key to Tauri's own reader of the fields' common type, and each field
 * read from its key with nothing else. Inputs: the type name, its `{ field, key }` list and the
 * fields' type. Output: the impl's text. */
function canonicalCommandArg(type, reads, fieldType) {
  return `impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for ${type} {
    fn from_command(command: tauri::ipc::CommandItem<'de, R>) -> Result<Self, tauri::ipc::InvokeError> {
        let tauri::ipc::CommandItem { plugin, name, message, acl, .. } = command;
        let read = |key| {
            <${fieldType} as tauri::ipc::CommandArg<'de, R>>::from_command(tauri::ipc::CommandItem { plugin, name, key, message, acl })
        };
        Ok(Self { ${reads.map(({ field, key }) => `${field}: read("${key}")?`).join(", ")} })
    }
}`;
}

/** Reads the argument types lib.rs parses itself through `tauri::ipc::CommandArg`: each struct
 * field and the top-level key it is read from, the way Tauri hands them over. Every impl of
 * `CommandArg`, under any path, must be token for token the canonical one for its struct.
 * Input: the lib.rs source. Output: a map from type name to its `{ field, key, type }` list in
 * field order; throws for any other impl, which the bridge could not parse as the app does. */
export function commandArgs(source) {
  const found = new Map();
  const heads = source.match(/^impl\b[^{\n]*\bCommandArg\b[^{\n]*\{$/gm) ?? [];
  for (const [impl, type] of source.matchAll(/^impl\b[^{\n]*\bCommandArg\b[^{\n]*\bfor (\w+) \{\n[\s\S]*?^\}/gm)) {
    const refused = new Error(`routes: ${type} reads its fields in a way the generator cannot follow`);
    const struct = source.match(new RegExp(`^(?:pub(?:\\([\\w:]+\\))? )?struct ${type} \\{\\n([\\s\\S]*?)^\\}`, "m"));
    const fields = splitTop((struct?.[1] ?? "").replace(/^\s*\/\/\/.*$/gm, "")).map((field) => {
      const colon = field.indexOf(":");
      return { field: field.slice(0, colon).replace(/^pub(?:\([\w:]+\))? /, "").trim(), type: field.slice(colon + 1).trim().replace(/\s+/g, " ") };
    });
    const keys = new Map([...impl.matchAll(/\b(\w+): read\("(\w+)"\)\?/g)].map(([, field, key]) => [field, key]));
    const reads = fields.map(({ field }) => ({ field, key: keys.get(field) }));
    const types = new Set(fields.map((field) => field.type));
    if (fields.length === 0 || types.size !== 1 || reads.some(({ key }) => !key) || new Set(keys.values()).size !== fields.length) throw refused;
    const expected = tokens(canonicalCommandArg(type, reads, [...types][0]));
    const actual = tokens(impl);
    if (actual.length !== expected.length || actual.some((token, index) => token !== expected[index])) throw refused;
    found.set(type, fields.map(({ field, type: fieldType }) => ({ field, key: keys.get(field), type: fieldType })));
  }
  assert.equal(found.size, heads.length, "every CommandArg impl of lib.rs was read to its end");
  return found;
}

/** Reads every `#[tauri::command]` of lib.rs with its parameters and body; a parameter of a
 * type lib.rs reads through its own `CommandArg` carries the keys it reads.
 * Input: the lib.rs source. Output: one record per command, in source order. */
export function commands(source) {
  const custom = commandArgs(source);
  const found = [];
  const head = /#\[tauri::command\]\s*(?:pub\s+)?(?:async\s+)?fn\s+(desktop_\w+)\s*\(/g;
  for (let match = head.exec(source); match; match = head.exec(source)) {
    let index = head.lastIndex;
    let depth = 1;
    while (depth > 0) {
      const char = source[index];
      if (char === "(") depth += 1;
      else if (char === ")") depth -= 1;
      index += 1;
    }
    const params = source.slice(head.lastIndex, index - 1);
    const open = source.indexOf("{", index);
    let close = open + 1;
    depth = 1;
    while (depth > 0) {
      const char = source[close];
      if (char === "{") depth += 1;
      else if (char === "}") depth -= 1;
      close += 1;
    }
    const body = source.slice(open + 1, close - 1).split("\n").map((line) => line.trim()).filter(Boolean).join(" ");
    found.push({ name: match[1], params: splitTop(params).map((param) => {
      const colon = param.indexOf(":");
      const type = param.slice(colon + 1).trim().replace(/\s+/g, " ");
      return { name: param.slice(0, colon).trim(), type, ...(custom.has(type) && { keys: custom.get(type) }) };
    }), body });
    head.lastIndex = close;
  }
  return found;
}

/** Reads the bridge's Rust sources, bridge.rs and every file under bridge/, whose impl blocks
 * say which methods a route may call. Input: none. Output: the sources joined in path order. */
export function bridgeSource() {
  const nested = readdirSync(BRIDGE, { recursive: true }).filter((name) => name.endsWith(".rs")).sort();
  assert.ok(nested.length > 0, "the bridge has module files");
  assert.ok(nested.length <= BRIDGE_FILES_MAX, `the bridge has at most ${BRIDGE_FILES_MAX} module files`);
  return [`${BRIDGE}.rs`, ...nested.map((name) => join(BRIDGE, name))].map((file) => readFileSync(file, "utf8")).join("\n");
}

/** Lists the methods each bridge type defines in its own `impl` blocks, so a route is checked
 * against the type it calls, not a bare method name. Input: Rust source. Output: a map from
 * `NativeBridge` and `BridgeQueue` to maps from method name to `{ async, pub, result }`. */
export function methods(source) {
  const found = new Map(IMPL_TYPES.map((type) => [type, new Map()]));
  const heads = source.match(/^impl (?:NativeBridge|BridgeQueue) \{$/gm) ?? [];
  let blocks = 0;
  for (const [, type, body] of source.matchAll(/^impl (NativeBridge|BridgeQueue) \{\n([\s\S]*?)^\}/gm)) {
    blocks += 1;
    for (const [signature, visibility, asynchronous, name] of body.matchAll(/^ {4}(pub(?:\([\w:]+\))? )?(async )?fn (\w+)\b[^{;]*/gm)) {
      const method = { async: Boolean(asynchronous), pub: visibility === "pub ", result: /->\s*Result</.test(signature) };
      const earlier = found.get(type).get(name);
      if (earlier && JSON.stringify(earlier) !== JSON.stringify(method)) {
        throw new Error(`routes: ${type}::${name} is defined twice with different signatures`);
      }
      found.get(type).set(name, method);
    }
  }
  assert.equal(blocks, heads.length, "every impl block of a bridge type was read to its end");
  return found;
}

let real;

/** Reads the app's own bridge impl blocks once per process, the methods every caller that
 * names none checks routes against. Input: none. Output: `methods` of `bridgeSource()`. */
function bridgeMethods() {
  real ??= methods(bridgeSource());
  return real;
}

/** Finds the end of the call whose argument list opens at `open`.
 * Inputs: the body and the index of the call's `(`. Output: the index just past its `)`. */
function callEnd(body, open) {
  assert.equal(body[open], "(", "a call opens with a parenthesis");
  let depth = 0;
  for (let index = open; index < body.length; index += 1) {
    if (body[index] === "(") depth += 1;
    else if (body[index] === ")") depth -= 1;
    if (depth === 0) return index + 1;
  }
  throw new Error(`routes: a call in ${body} never closes`);
}

/** Checks each bridge call of a route body against the impl blocks of the type it calls:
 * `state.` is the queue; `lock(state)`, `read(state)` and guards bound from them are the bridge.
 * The method must be a public one of that type, and `.await` may follow a sync method only when
 * it gives a future rather than a Result. A name both types define, called on any other
 * receiver, is refused. Inputs: the command name, the body and `methods` output. Throws. */
function resolve(name, body, impls) {
  const guards = [...body.matchAll(/(?<![.\w])let (?:mut )?(\w+) = (?:lock|read)\(state\)\s*\.await\?;/g)].map((match) => match[1]);
  const receivers = [
    ["BridgeQueue", /(?<![.\w])state\s*\.(\w+)\(/g],
    ["NativeBridge", /(?<![.\w])(?:lock|read)\(state\)\s*\.await\?\s*\.(\w+)\(/g],
    ...guards.map((guard) => ["NativeBridge", new RegExp(`(?<![.\\w])${guard}\\s*\\.(\\w+)\\(`, "g")]),
  ];
  const typed = new Set();
  for (const [type, pattern] of receivers) {
    for (const match of body.matchAll(pattern)) {
      const open = match.index + match[0].length - 1;
      const method = impls.get(type).get(match[1]);
      if (!method?.pub) throw new Error(`routes: ${name} calls ${type}::${match[1]}, which no impl ${type} defines as pub`);
      const awaited = /^\s*\.await\b/.test(body.slice(callEnd(body, open)));
      if (awaited && !method.async && method.result) {
        throw new Error(`routes: ${name} awaits ${type}::${match[1]}, which is not async and gives a Result`);
      }
      typed.add(open);
    }
  }
  for (const match of body.matchAll(/\.(\w+)\(/g)) {
    const open = match.index + match[0].length - 1;
    const shared = IMPL_TYPES.every((type) => impls.get(type).has(match[1]));
    if (shared && !typed.has(open)) {
      throw new Error(`routes: ${name} calls ${match[1]}, which both ${IMPL_TYPES.join(" and ")} define, on a receiver the generator cannot type`);
    }
  }
}

/** Decides how the end-to-end bridge serves one command: routed with its arguments and body,
 * or left out with the reason (it needs the native window, terminals, updates or a picker).
 * A body that is one call on the app's bridge queue calls the end-to-end bridge's queue.
 * Inputs: one command record and `methods` output, the app's bridge when omitted. Output:
 * `{ route }` or `{ skip }`; throws when a routed call does not match its type's impl block. */
export function plan(command, impls = bridgeMethods()) {
  assert.ok(IMPL_TYPES.every((type) => impls?.get(type) instanceof Map), "plan reads the bridge's impl blocks");
  const prefix = ["main_window(&window)?;", "terminal_window(&window)?;"].find((guard) => command.body.startsWith(guard));
  if (prefix === undefined) return { skip: "no window check" };
  const args = command.params.filter((param) => param.name !== "window" && !/^State<'_, NativeState>$/.test(param.type));
  if (!command.params.some((param) => param.name === "window")) return { skip: "no window parameter" };
  if (args.some((param) => NATIVE_PARAM.test(param.type))) return { skip: "native parameter" };
  let body = command.body.slice(prefix.length).replace("ensure_default_engine().await?;", "").replace(/\s+/g, " ").trim();
  const queued = QUEUE_CALL.test(body);
  if (queued) body = body.replace(/^state\.0\./, "state.");
  if (NATIVE_ONLY.test(body)) return { skip: "native-only body" };
  body = body.replaceAll("lock(&state)", "lock(state)").replaceAll("read(&state)", "read(state)");
  const rest = (queued ? body.slice("state.".length) : body).replaceAll("lock(state)", "").replaceAll("read(state)", "");
  if (/\bstate\b/.test(rest)) return { skip: "uses the state directly" };
  resolve(command.name, body, impls);
  return { route: { name: command.name, args: args.map((arg) => ({ name: arg.name, type: arg.type.replaceAll(/\bbridge::/g, "crazyagent_desktop::bridge::"), ...(arg.keys && { keys: arg.keys }) })), body } };
}

/** Reads the leaf names lib.rs imports, mapped to paths the bridge binary can name.
 * Input: the lib.rs source. Output: a map from leaf name to its full path. */
function imports(source) {
  const paths = new Map();
  for (const match of source.matchAll(/^use\s+([^;]+);/gm)) {
    const text = match[1].replace(/\s+/g, " ").trim();
    const brace = text.indexOf("{");
    const base = brace === -1 ? text.slice(0, text.lastIndexOf("::")) : text.slice(0, brace).replace(/::$/, "");
    const leaves = brace === -1 ? [text.slice(text.lastIndexOf("::") + 2)] : splitTop(text.slice(brace + 1, text.lastIndexOf("}")));
    const root = base.startsWith("bridge") ? `crazyagent_desktop::${base}` : base;
    if (!/^(crazyagent|crazyagent_desktop)\b/.test(root)) continue;
    for (const leaf of leaves) if (/^\w+$/.test(leaf)) paths.set(leaf, `${root}::${leaf}`);
  }
  return paths;
}

/** Renders the bridge binary's route table from lib.rs: one arm per routed command that parses
 * its camelCase arguments like Tauri and runs the command body on the shared bridge. An argument
 * lib.rs reads through its own `CommandArg` gets a same-named struct in the arm that takes each
 * field from the key the app reads, flattened so those keys sit beside the other arguments.
 * Inputs: the lib.rs source and `methods` output, the app's bridge when omitted. Output: the
 * routes.rs text. */
export function render(source, impls = bridgeMethods()) {
  const routes = commands(source).map((command) => plan(command, impls)).filter((planned) => planned.route).map((planned) => planned.route);
  const arms = routes.map(({ name, args, body }) => {
    const structs = args.filter((arg) => arg.keys).map((arg) => `            #[derive(serde::Deserialize)]
            struct ${arg.type} {
${arg.keys.map(({ field, key, type }) => `                #[serde(rename = "${key}")]\n                ${field}: ${type},\n`).join("")}            }
`).join("");
    const fields = args.map((arg) => `${arg.keys ? "                #[serde(flatten)]\n" : ""}                ${arg.name}: ${arg.type},\n`).join("");
    const parse = args.length === 0 ? "" : `${structs}            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
${fields}            }
            let Args { ${args.map((arg) => arg.name).join(", ")} } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
`;
    return `        "${name}" => {
${parse}            let result: Result<_, BridgeError> = async { ${body} }.await;
            routed(result)
        }
`;
  }).join("");
  const used = new Set(arms.match(/\b[A-Z]\w*/g) ?? []);
  const uses = [...imports(source)].filter(([leaf]) => used.has(leaf) && leaf !== "BridgeError").map(([, path]) => `    use ${path};\n`).sort().join("");
  return `#[rustfmt::skip]
pub(super) async fn route(state: &super::Shared, command: &str, args: serde_json::Value) -> super::Routed {
    use super::{lock, parse, read, routed, Routed};
    use crazyagent_desktop::bridge::BridgeError;
${uses}    match command {
${arms}        _ => Routed::Unknown,
    }
}
`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  mkdirSync(dirname(ROUTES), { recursive: true });
  writeFileSync(ROUTES, render(readFileSync(LIB, "utf8"), methods(bridgeSource())));
}
