import { useState } from "react";
import { failureCopy } from "../../api-projects";
import type { MessageSettings } from "../../engine-client";
import { DEFAULT_MESSAGE_SETTINGS, MESSAGE_SETTING_FIELDS, changeMessageSetting, fieldValue, messageSettingsOf } from "../../api-messages";
import { Segmented } from "../../ui";
import { changeNote } from "../team/controls";
import { NotReady } from "./shared";
import type { ProjectData } from "./shared";

/** The Messages section: the six settings of the sub-agents' Messages timelines, each with every
 * choice and the proposed default named. A choice changes its one setting against what the
 * engine stores now; when another client changed the settings since this view was read, it
 * saves nothing and shows the stored ones. The section shows the outcome until the next team
 * read, then what the engine stored. Input: the project data. Output: the section. */
export function MessagesSection({ data }: { data: ProjectData }) {
  const [pending, setPending] = useState<{ settings: MessageSettings; at: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const read = data.team.result;
  const saved = read?.kind === "ok" ? messageSettingsOf(read.value) : null;
  const shown = pending !== null && data.team.at < pending.at ? pending.settings : saved;

  async function choose(key: keyof MessageSettings, value: string) {
    if (busy || shown === null) return;
    setBusy(true);
    const change = await changeMessageSetting(data.id, shown, key, value);
    setBusy(false);
    if (change.kind === "elsewhere") {
      setPending({ settings: change.settings, at: Date.now() });
      setNote("Messages settings changed elsewhere, so nothing was saved. This shows what's saved now; choose again.");
    } else {
      const message = changeNote(change.result, "Messages settings");
      setPending(message || change.kind === "unread" ? null : { settings: change.settings, at: Date.now() });
      setNote(message);
    }
    data.reloadTeam();
  }

  return <div className="pj-stack">
    <h2>Messages</h2>
    <p className="pj-secondary">Each sub-agent's Messages show its tasks, what the master and you told it, and its questions, progress and results, newest first. Open them from the Agents tab or the team graph.</p>
    {read === undefined && <p className="pj-muted" role="status">Loading...</p>}
    {read?.kind === "not_ready" && <NotReady what="the live team isn't in this build." />}
    {read?.kind === "error" && <p className="pj-error" role="alert">Couldn't load the team. {failureCopy(read)}</p>}
    {read?.kind === "ok" && shown === null && <NotReady what="Messages settings aren't in this build." />}
    {shown !== null && MESSAGE_SETTING_FIELDS.map((field) => {
      const proposed = field.options.find((option) => option.value === fieldValue(DEFAULT_MESSAGE_SETTINGS, field.key));
      return <div className="pj-field" key={field.key}>
        <span className="pj-label" aria-hidden="true">{field.label}</span>
        <Segmented label={field.label} value={fieldValue(shown, field.key)} options={[...field.options]} onValue={(value) => void choose(field.key, value)} />
        <p className="pj-help">{field.help} Recommended: {proposed?.label}.</p>
      </div>;
    })}
    {note && <p className="pj-note warn" role="status">{note}</p>}
  </div>;
}
