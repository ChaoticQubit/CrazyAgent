import { readFileSync, readdirSync } from "node:fs";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/**
 * The native sources that may declare a window command: lib.rs, then every other `.rs` file
 * of src-tauri/src in name order, each a module of the crate root.
 * Output: their texts joined, lib.rs first.
 */
export function nativeSources() {
  const folder = new URL("../src-tauri/src/", import.meta.url);
  const files = readdirSync(folder).filter((name) => name.endsWith(".rs") && name !== "lib.rs").sort();
  return [read("src-tauri/src/lib.rs"), ...files.map((name) => read(`src-tauri/src/${name}`))].join("\n");
}

/**
 * The rule a window command meets in the native sources: `#[tauri::command]` right above its
 * `fn`, private or `pub(crate)`, async or not, with optional generics.
 * Input: the command name. Output: the pattern.
 */
export function commandFn(command) {
  return new RegExp(`#\\[tauri::command\\]\\s*(?:pub\\(crate\\) )?(?:async )?fn ${command}(?:<[^>]*>)?\\(`);
}

/**
 * The commands with no declaration meeting [`commandFn`] in a source text. Inputs: the
 * commands and the text. Output: the commands that fail the rule.
 */
export function unbound(commands, source) {
  return commands.filter((command) => !commandFn(command).test(source));
}
