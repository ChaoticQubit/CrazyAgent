import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

export const LIB = fileURLToPath(new URL("../../src-tauri/src/lib.rs", import.meta.url));
export const ROUTES = fileURLToPath(new URL("../../src-tauri/e2e_bridge/routes.rs", import.meta.url));

const NATIVE_ONLY = /\bbridge::|\bupdates::|\bterminals\b|\bapp\b|NativeBridge::|Arc::|default_engine\(|SOCKET_OVERRIDE|state\.0/;
const NATIVE_PARAM = /State<|AppHandle|Channel<|WebviewWindow/;

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

/** Reads every `#[tauri::command]` of lib.rs with its parameters and body.
 * Input: the lib.rs source. Output: one record per command, in source order. */
export function commands(source) {
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
      return { name: param.slice(0, colon).trim(), type: param.slice(colon + 1).trim().replace(/\s+/g, " ") };
    }), body });
    head.lastIndex = close;
  }
  return found;
}

/** Decides how the end-to-end bridge serves one command: routed with its arguments and body,
 * or left out with the reason (it needs the native window, terminals, updates or a picker).
 * Input: one command record. Output: `{ route }` or `{ skip }`. */
export function plan(command) {
  const prefix = "main_window(&window)?;";
  if (!command.body.startsWith(prefix)) return { skip: "no main-window check" };
  const args = command.params.filter((param) => param.name !== "window" && !/^State<'_, NativeState>$/.test(param.type));
  if (!command.params.some((param) => param.name === "window")) return { skip: "no window parameter" };
  if (args.some((param) => NATIVE_PARAM.test(param.type))) return { skip: "native parameter" };
  let body = command.body.slice(prefix.length).replace("ensure_default_engine().await?;", "").replace(/\s+/g, " ").trim();
  if (NATIVE_ONLY.test(body)) return { skip: "native-only body" };
  body = body.replaceAll("lock(&state)", "lock(state)").replaceAll("read(&state)", "read(state)");
  if (/\bstate\b/.test(body.replaceAll("lock(state)", "").replaceAll("read(state)", ""))) return { skip: "uses the state directly" };
  return { route: { name: command.name, args: args.map((arg) => ({ name: arg.name, type: arg.type.replaceAll(/\bbridge::/g, "crazyagent_desktop::bridge::") })), body } };
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
 * its camelCase arguments like Tauri and runs the command body on the shared bridge.
 * Input: the lib.rs source. Output: the routes.rs text. */
export function render(source) {
  const routes = commands(source).map(plan).filter((planned) => planned.route).map((planned) => planned.route);
  const arms = routes.map(({ name, args, body }) => {
    const fields = args.map((arg) => `                ${arg.name}: ${arg.type},\n`).join("");
    const parse = args.length === 0 ? "" : `            #[derive(serde::Deserialize)]
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
  writeFileSync(ROUTES, render(readFileSync(LIB, "utf8")));
}
