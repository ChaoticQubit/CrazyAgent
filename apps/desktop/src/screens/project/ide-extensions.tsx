import { useEffect, useState } from "react";
import { call, knownError, namedFailure, type Result } from "../../api";
import { ConfirmDialog } from "../../ui";
import type { ProjectData } from "./shared";

/** One install or update the project's IDE asked the owner to confirm: the extension id,
 * the version or "latest", and the version approved now for an update. */
export interface ExtensionAsk {
  id: string;
  version: string;
  current: string | null;
}

/** One extension the owner approved: the exact version and the sha256 it is pinned to. */
export interface ApprovedExtension {
  id: string;
  version: string;
  sha256: string;
  approved_ms: number;
}

const POLL_MS = 3000;

const REFUSALS: Record<string, string> = {
  ide_extension_ai_locked: "CrazyAgent keeps AI features out of the IDE, so this extension can't be installed.",
  ide_extension_ask_missing: "The IDE's request expired. Ask again from the IDE.",
  ide_extension_hash_mismatch: "The download didn't match the checksum Open VSX lists, so it wasn't approved.",
  ide_extension_not_found: "Open VSX doesn't list this extension or version.",
  ide_extension_source_refused: "Open VSX pointed the download somewhere else, so it wasn't approved.",
  ide_extension_too_large: "This extension is larger than CrazyAgent installs.",
  ide_extensions_full: "CrazyAgent already has as many approved extensions as it allows.",
};

/** Reads the extension asks of one project's IDE. Input: the project id. Output: the asks. */
export function loadExtensionAsks(projectId: string): Promise<Result<ExtensionAsk[]>> {
  return call<ExtensionAsk[]>("desktop_ide_extension_asks", { projectId });
}

/** Approves one ask: the engine downloads it from Open VSX, checks its sha256 and pins that
 * exact version. Inputs: the project id and the ask. Output: the pin. */
export function approveExtension(projectId: string, ask: ExtensionAsk): Promise<Result<ApprovedExtension>> {
  return call<ApprovedExtension>("desktop_ide_extension_approve", { projectId, id: ask.id, version: ask.version });
}

/** Drops one ask without approving it. Inputs: the project id and the ask. */
export function declineExtension(projectId: string, ask: ExtensionAsk): Promise<Result<void>> {
  return call<void>("desktop_ide_extension_decline", { projectId, id: ask.id, version: ask.version });
}

/** One plain sentence for a refused extension request. Input: the refusal. Output: the sentence. */
export function extensionRefusal(refusal: { code: string; cause?: string }): string {
  const sentence = Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : knownError(refusal.code);
  return namedFailure(sentence, refusal.code, refusal.cause);
}

/** The confirm's title and its button for one ask. Input: the ask. Output: title and label. */
export function askWords(ask: ExtensionAsk): { title: string; confirm: string } {
  const version = ask.version === "latest" ? "the newest version" : ask.version;
  if (ask.current) return { title: `Update ${ask.id} from ${ask.current} to ${version}?`, confirm: "Update" };
  return { title: `Install ${ask.id} ${version}?`, confirm: "Install" };
}

/** Asks the owner to confirm each extension install or update the project's IDE requested,
 * one at a time, while the project page is open. Approving downloads the VSIX from Open VSX,
 * checks its sha256 and pins that version; the IDE installs it when it next starts. Not now
 * drops the ask. Input: the project data. Output: the dialog, or nothing with no ask. */
export function IdeExtensionConfirm({ data }: { data: ProjectData }) {
  const [asks, setAsks] = useState<ExtensionAsk[]>([]);
  const [busy, setBusy] = useState(false);
  const [checked, setChecked] = useState(0);

  useEffect(() => {
    let live = true;
    let timer = 0;
    const read = async () => {
      const result = await loadExtensionAsks(data.id);
      if (!live) return;
      if (result.kind === "ok") setAsks(result.value);
      if (result.kind !== "not_ready") timer = window.setTimeout(() => void read(), POLL_MS);
    };
    void read();
    return () => { live = false; window.clearTimeout(timer); };
  }, [data.id, checked]);

  const ask = asks[0];
  if (!ask) return null;
  const words = askWords(ask);

  async function settle(approve: boolean) {
    setBusy(true);
    const result = approve ? await approveExtension(data.id, ask) : await declineExtension(data.id, ask);
    setBusy(false);
    setAsks((left) => left.filter((item) => item !== ask));
    setChecked((count) => count + 1);
    if (result.kind === "not_ready") return data.notify("Not ready yet: extension approvals aren't in this build.");
    if (result.kind === "error") return data.notify(extensionRefusal(result));
    if (approve) data.notify(`Approved ${ask.id} ${(result.value as ApprovedExtension).version}. The IDE installs it when it next starts.`);
  }

  return <ConfirmDialog open title={words.title} confirmLabel={words.confirm} cancelLabel="Not now" pending={busy}
    onCancel={() => void settle(false)} onConfirm={() => void settle(true)}>
    The IDE asked for this extension. CrazyAgent downloads it from Open VSX, checks it against the published checksum and pins
    that exact version. It never updates on its own; each update asks you again.
  </ConfirmDialog>;
}
