const vscode = require("vscode");
const fs = require("fs");
const path = require("path");

const KEYS = [
  "chat.disableAIFeatures",
  "files.readonlyInclude",
  "task.allowAutomaticTasks",
  "php.validate.executablePath",
  "git.enabled",
  "security.workspace.trust.enabled",
];
const SETTLE_MS = 5000;
const REPORT = "verdict5.json";

/** What one key is for a scope: its effective value and whether a folder set it.
 * Inputs: the configuration and the key. Output: the entry. */
function entry(config, key) {
  const info = config.inspect(key) || {};
  return { effective: config.get(key), folderValue: info.workspaceFolderValue !== undefined };
}

/** Tries to read a folder's own settings file from this extension host, which runs inside
 * the IDE server's sandbox. Input: the folder path. Output: "read" or the error code. */
function readSettings(folder) {
  try {
    fs.readFileSync(path.join(folder, ".vscode", "settings.json"));
    return "read";
  } catch (error) {
    return error.code || "failed";
  }
}

/** Opens every planted PHP file so a planted validator would run, waits for settings to
 * settle, then writes what each workspace folder sees into the probe's storage folder.
 * Input: the extension context. Output: none. */
async function activate(context) {
  for (const folder of vscode.workspace.workspaceFolders || []) {
    const php = vscode.Uri.joinPath(folder.uri, "verdict5.php");
    try {
      await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(php), { preview: false });
    } catch {
      continue;
    }
  }
  await new Promise((done) => setTimeout(done, SETTLE_MS));
  const window = {};
  for (const key of KEYS) window[key] = entry(vscode.workspace.getConfiguration(), key);
  const folders = (vscode.workspace.workspaceFolders || []).map((folder) => {
    const config = vscode.workspace.getConfiguration(undefined, folder.uri);
    const keys = {};
    for (const key of KEYS) keys[key] = entry(config, key);
    return { name: folder.name, path: folder.uri.fsPath, keys, settings: readSettings(folder.uri.fsPath) };
  });
  const storage = context.globalStorageUri.fsPath;
  fs.mkdirSync(storage, { recursive: true });
  fs.writeFileSync(path.join(storage, REPORT), JSON.stringify({ trusted: vscode.workspace.isTrusted, window, folders }));
}

/** Nothing to stop. */
function deactivate() {}

module.exports = { activate, deactivate };
