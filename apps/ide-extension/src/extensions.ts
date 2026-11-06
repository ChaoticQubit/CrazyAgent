import * as vscode from "vscode";
import type { Engine, Reply } from "./engine.js";
import type { Feature } from "./extension.js";
import { unconfirmed } from "./scm-model.js";

/** One extension at one version, or `latest` for the newest Open VSX lists. */
interface Version {
  id: string;
  version: string;
}

/** One waiting ask or available update with the version approved now, if any. */
interface Pending extends Version {
  current: string | null;
}

/** What the engine lists for this project: approved extensions, asks and updates. */
interface Listing {
  approved: Version[];
  asks: Pending[];
  updates: Pending[];
}

/** The engine's answer to an ask: approved already, sent to the desktop confirm, or dismissed. */
type Asked = { state: "approved" | "confirm" | "dismissed" };

const REQUEST = "crazyagent.extensions.request";
const UPDATES = "crazyagent.extensions.updates";
const ASK_UPDATE = "Ask to update";
const DISMISS = "Dismiss";

/**
 * Reads `publisher.name` or `publisher.name@version` as typed in the install box; without a
 * version it asks for the newest. Input: the text. Output: the id in lowercase and the
 * version, or undefined when the text is neither.
 */
export function parseRequest(text: string): Version | undefined {
  const [id = "", version = "latest", ...rest] = text.trim().split("@");
  const named = /^[A-Za-z0-9_-]{1,128}\.[A-Za-z0-9_-]{1,128}$/.test(id);
  const versioned = /^[A-Za-z0-9+-][A-Za-z0-9.+-]{0,63}$/.test(version);
  if (rest.length > 0 || !named || !versioned) return undefined;
  return { id: id.toLowerCase(), version };
}

/**
 * The line the IDE shows for the engine's answer to an ask. Input: the reply.
 * Output: one plain sentence.
 */
export function answerLine(reply: Reply<Asked>): string {
  if (reply.kind !== "ok") return unconfirmed(reply.code) ?? `CrazyAgent refused this (${reply.code}).`;
  if (reply.data.state === "approved") return "This version is approved. The IDE installs it when it next starts.";
  if (reply.data.state === "dismissed") return "Dismissed. It shows again if a newer version comes out.";
  return "Confirm this in the CrazyAgent app. Nothing installs until you approve it there.";
}

async function show(pending: Promise<Reply<Asked>>): Promise<void> {
  try {
    void vscode.window.showInformationMessage(answerLine(await pending));
  } catch (error) {
    void vscode.window.showErrorMessage(`CrazyAgent didn't answer (${(error as Error).message}).`);
  }
}

async function request(engine: Engine): Promise<void> {
  const text = await vscode.window.showInputBox({
    title: "Install an extension from Open VSX",
    prompt: "publisher.name for the newest version, or publisher.name@version for an exact one",
    validateInput: (value) => (parseRequest(value) ? undefined : "Type publisher.name or publisher.name@version."),
  });
  const wanted = text === undefined ? undefined : parseRequest(text);
  if (wanted === undefined) return;
  await show(engine.request<Asked>("ide.extension_request", { id: wanted.id, version: wanted.version }));
}

async function listing(engine: Engine): Promise<Listing | undefined> {
  try {
    const reply = await engine.request<Listing>("ide.extensions");
    return reply.kind === "ok" ? reply.data : undefined;
  } catch {
    return undefined;
  }
}

async function updates(engine: Engine): Promise<void> {
  const listed = await listing(engine);
  if (listed === undefined) {
    void vscode.window.showErrorMessage("CrazyAgent couldn't read the extension updates.");
    return;
  }
  if (listed.updates.length === 0) {
    void vscode.window.showInformationMessage("No extension updates are available.");
    return;
  }
  const picked = await vscode.window.showQuickPick(
    listed.updates.map((update) => ({
      label: update.id,
      description: `${update.current ?? "not approved"} to ${update.version}`,
      update,
    })),
    { title: "Extension updates", placeHolder: "Nothing updates until the owner approves it in the CrazyAgent app" },
  );
  if (picked === undefined) return;
  const choice = await vscode.window.showQuickPick([ASK_UPDATE, DISMISS], {
    title: `${picked.update.id} ${picked.update.version}`,
  });
  if (choice === undefined) return;
  const { id, version } = picked.update;
  await show(engine.request<Asked>("ide.extension_decide", { id, version, approve: choice === ASK_UPDATE }));
}

async function announce(engine: Engine): Promise<void> {
  const listed = await listing(engine);
  const count = listed?.updates.length ?? 0;
  if (count === 0) return;
  const noun = count === 1 ? "update is" : "updates are";
  const open = await vscode.window.showInformationMessage(
    `${count} extension ${noun} available. Nothing updates until you approve it.`,
    "Show updates",
  );
  if (open !== undefined) await updates(engine);
}

/**
 * Owner-approved Open VSX extensions: a command to ask for an install, a command to see
 * available updates and ask for or dismiss one, and a notice of updates when the IDE starts.
 * Every install or update is approved in the CrazyAgent app, never here.
 * Inputs: the extension context and the engine bridge. Output: none.
 */
export const extensions: Feature = (context, engine) => {
  context.subscriptions.push(
    vscode.commands.registerCommand(REQUEST, () => request(engine)),
    vscode.commands.registerCommand(UPDATES, () => updates(engine)),
  );
  void announce(engine);
};
