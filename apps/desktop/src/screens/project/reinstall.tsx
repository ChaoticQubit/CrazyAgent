import { useEffect, useState } from "react";
import { listAccounts } from "../../api";
import { dismissReinstallOffer, loadReinstallOffer } from "../../api-intake";
import type { ReinstallOffer } from "../../engine-client";
import { useLoad } from "../../state";
import { displayText } from "../conversation";

/** What the notice adds when no account is set up: the master can't run until one is. */
export const REINSTALL_SIGN_IN = "Add an account first; the master can reinstall them once one is signed in.";

/** What a project's reinstall notice says: the folders a job left in iCloud, the first three
 * named and the rest counted, and why. Input: the offer. Output: the sentence. */
export function reinstallLine(offer: ReinstallOffer): string {
  return offer.folders.map(displayText).join(", ");
}

/** The reinstall notice as drawn: the sentence, Ask the master when an account is set up, else
 * the sign-in sentence and Open accounts, and Dismiss. Inputs: the offer, whether an account is
 * set up, a dismiss problem, a busy flag and the handlers. Output: the notice. */
export function ReinstallView({ offer, signedIn, problem, busy, onAsk, onDismiss, onAccounts }: {
  offer: ReinstallOffer;
  signedIn: boolean;
  problem: string;
  busy: boolean;
  onAsk: () => void;
  onDismiss: () => void;
  onAccounts: () => void;
}) {
  return <div className="pj-note" role="status">
    <p>{reinstallLine(offer)}</p>
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <div className="pj-row">
      <button type="button" className="btn" disabled={busy || !signedIn} onClick={signedIn ? onAsk : onAccounts}>Ask the master</button>
      <button type="button" className="btn" disabled={busy} onClick={onDismiss}>Dismiss</button>
    </div>
  </div>;
}

/** The project's reinstall notice: reads the offer the engine keeps for the project's folder;
 * Ask hands its drafted message to the master's input and keeps the offer, Dismiss deletes it.
 * Inputs: the project, what Ask does with the draft and how to open the accounts page.
 * Output: the notice, or nothing when there is no offer. */
export function ReinstallNotice({ projectId, onAsk, onAccounts }: {
  projectId: string;
  onAsk: (draft: string) => void;
  onAccounts: () => void;
}) {
  const [offer, setOffer] = useState<ReinstallOffer | null>(null);
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);
  const [accounts] = useLoad(listAccounts, [], "accounts");
  useEffect(() => {
    let live = true;
    setOffer(null);
    setProblem("");
    void loadReinstallOffer(projectId).then((read) => {
      if (live && read.kind === "ok") setOffer(read.value);
    });
    return () => { live = false; };
  }, [projectId]);
  if (offer === null) return null;
  const signedIn = !(accounts.kind === "ok" && accounts.value.length === 0);
  const dismiss = async () => {
    setBusy(true);
    const done = await dismissReinstallOffer(projectId);
    setBusy(false);
    if (done.kind === "ok") return setOffer(null);
    setProblem(done.kind === "error" ? done.message : "Dismissing isn't in this build.");
  };
  return <ReinstallView offer={offer} signedIn={signedIn} problem={problem} busy={busy}
    onAsk={() => onAsk(offer.draft)} onDismiss={() => void dismiss()} onAccounts={onAccounts} />;
}
