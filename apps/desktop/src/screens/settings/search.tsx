import { useEffect, useState } from "react";
import { type Result } from "../../api";
import {
  SEARCH_POLL_MS, SEARCH_STALE, appPluginRefusal, cancelSearchModel, choiceValue, deleteSearchModel, downloadSearchModel, hostOf, httpOption, loadSearch, megabytes, problemText,
  saveSearch, searchChoice, searchPolling, type ModelState, type SearchChoice, type SearchOption, type SearchSave, type SearchView,
} from "../../api-app-plugins";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, Dialog, LoadError, NotReady, Notice, RadioCard, SettingLine, SettingSection, Skeleton } from "../../ui";
import { runsLine } from "./app-plugins";
import "../project/projects.css";

const NOT_READY = "This isn't ready in this build yet.";
const CHOICE = "search-choice";

/** What Search has open: nothing, the confirm before an HTTP plugin is chosen, the confirm
 * before the built-in model downloads (holding the size it names, sent as confirm_bytes), or
 * the confirm before it is deleted. */
export type SearchDialog =
  | { kind: "none" }
  | { kind: "http"; option: SearchOption }
  | { kind: "download"; bytes: number }
  | { kind: "delete" };

/** Everything the Search view asks its container to do. */
export interface SearchActions {
  onChoose: (choice: SearchChoice) => void;
  onConfirmHttp: () => void;
  onDownload: () => void;
  onConfirmDownload: () => void;
  onCancelDownload: () => void;
  onDelete: () => void;
  onConfirmDelete: () => void;
  onClose: () => void;
  onRetry: () => void;
}

const TOP = <div className="settings-top"><div>
  <h2>Search</h2>
  <p>How agents' search over sessions and memories ranks what it finds. Full text always works; an embedding plugin adds matches by meaning.</p>
</div></div>;

function ModelLine({ model, busy, actions }: { model: ModelState; busy: boolean; actions: SearchActions }) {
  const size = megabytes(model.size_bytes);
  if (model.state === "downloading") {
    return <SettingLine wide label="Built-in model" help={`Downloading: ${megabytes(model.done_bytes)} of ${size}`}>
      <progress value={model.done_bytes} max={model.size_bytes} aria-label="Built-in model download" />
      <Button disabled={busy} onClick={actions.onCancelDownload}>Cancel download</Button>
    </SettingLine>;
  }
  if (model.state === "ready") {
    return <SettingLine wide label="Built-in model" help={`Ready: ${size} on this computer.`}>
      <Button variant="danger-text" disabled={busy} onClick={actions.onDelete}>Delete model</Button>
    </SettingLine>;
  }
  if (model.state === "failed") {
    return <SettingLine wide label="Built-in model" help={problemText(model.problem)}>
      <Button disabled={busy} onClick={actions.onDownload}>Download again</Button>
    </SettingLine>;
  }
  return <SettingLine wide label="Built-in model" help={`Not on this computer. A ${size} download, once; it then works offline.`}>
    <Button disabled={busy} onClick={actions.onDownload}>Download</Button>
  </SettingLine>;
}

function SearchDialogs({ dialog, model, busy, failure, actions }: { dialog: SearchDialog; model: ModelState; busy: boolean; failure: string; actions: SearchActions }) {
  const problem = failure ? <Notice tone="danger" alert>{failure}</Notice> : null;
  if (dialog.kind === "http") {
    const host = hostOf(dialog.option.runs.kind === "http" ? dialog.option.runs.url : "") ?? "";
    return <Dialog open title={`Send search text to ${host}?`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onConfirmHttp}>{`Send to ${host}`}</Button></>}>
      <div className="pj-stack-s">
        {problem}
        <p>{`Once chosen, CrazyAgent sends these to ${host} in the background:`}</p>
        <ul className="pj-rows" aria-label={`What leaves this computer for ${host}`}>{dialog.option.sends.map((line) => <li key={line}>{line}</li>)}</ul>
      </div>
    </Dialog>;
  }
  if (dialog.kind === "download") {
    return <Dialog open title="Download the built-in model?" onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Starting..." onClick={actions.onConfirmDownload}>Download</Button></>}>
      {problem}
      <p>{`${megabytes(dialog.bytes)}, once. CrazyAgent checks it before use; search uses full text until it is ready.`}</p>
    </Dialog>;
  }
  if (dialog.kind === "delete") {
    return <Dialog open title="Delete the built-in model?" onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="danger" pending={busy} pendingLabel="Deleting..." onClick={actions.onConfirmDelete}>Delete model</Button></>}>
      {problem}
      <p>{`Frees ${megabytes(model.size_bytes)}. Search with the built-in model stops until you download it again.`}</p>
    </Dialog>;
  }
  return null;
}

/** Settings, Search as a pure view: the title in every state, then Full text only and every
 * embedding plugin as choices (an unusable one off with its reason), the embedding progress of
 * a plugin choice, the built-in model's download state with its one action, and the open
 * confirm. Inputs: the Search page read, the open dialog, a busy flag, the last failure and
 * the actions. Output: the tab. */
export function SearchSettingsView({ search, dialog, busy, failure, actions }: {
  search: Loaded<SearchView>;
  dialog: SearchDialog;
  busy: boolean;
  failure: string;
  actions: SearchActions;
}) {
  if (search.kind === "loading") return <>{TOP}<Skeleton shape="line" count={4} /></>;
  if (search.kind === "not_ready") return <>{TOP}<NotReady what="Search settings" /></>;
  if (search.kind === "error") return <>{TOP}<LoadError message={appPluginRefusal(search)} onRetry={actions.onRetry} /></>;
  const page = search.value;
  const chosen = choiceValue(page.choice);
  const pick = (value: string) => { if (!busy && value !== chosen) actions.onChoose(searchChoice(value)); };
  return <>
    {TOP}
    <div className="pj set-groups">
      <SettingSection title="Rank search with">
        <SettingLine wide label="Rank search with" changed={chosen !== "plugin:local"}>
          {dialog.kind === "none" && failure && <Notice tone="danger" alert>{failure}</Notice>}
          <fieldset className="field" disabled={busy}>
            <legend className="visually-hidden">Rank search with</legend>
            <RadioCard name={CHOICE} value="full_text" checked={chosen === "full_text"} onChecked={pick} title="Full text only"
              details={["Exact words only. Nothing is downloaded or sent."]} />
            {page.options.map((option) => <RadioCard key={option.name} name={CHOICE} value={`plugin:${option.name}`} checked={chosen === `plugin:${option.name}`}
              disabled={!option.usable} onChecked={pick} title={option.title}
              details={[runsLine(option.runs), ...(option.usable || option.reason === null ? [] : [option.reason])]} />)}
          </fieldset>
          {page.choice.kind === "plugin" && <p className="muted small">{`Embedded ${page.progress.embedded} of ${page.progress.total} sessions and memories.`}</p>}
        </SettingLine>
      </SettingSection>
      <SettingSection title="Built-in model" about="The local embedding model runs on this computer. It is downloaded only after you confirm.">
        <ModelLine model={page.model} busy={busy} actions={actions} />
      </SettingSection>
    </div>
    <SearchDialogs dialog={dialog} model={page.model} busy={busy} failure={failure} actions={actions} />
  </>;
}

/** Settings, Search: reads the page, saves a choice by version (asking first, naming the host,
 * before an HTTP plugin is chosen), and downloads, cancels or deletes the built-in model only
 * after the owner confirms. A save another window beat reloads and says so. */
export function SearchSettings() {
  const session = useAppSession();
  const [search, reload] = useLoad(loadSearch);
  const [dialog, setDialog] = useState<SearchDialog>({ kind: "none" });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const polling = search.kind === "ok" && searchPolling(search.value);
  useEffect(() => {
    if (!polling) return;
    const timer = window.setInterval(reload, SEARCH_POLL_MS);
    return () => window.clearInterval(timer);
  }, [polling]);

  async function attempt<T>(work: () => Promise<Result<T>>): Promise<T | undefined> {
    setBusy(true);
    setFailure("");
    const result = session.noteResult(await work());
    setBusy(false);
    if (result.kind === "ok") return result.value;
    setFailure(result.kind === "not_ready" ? NOT_READY : appPluginRefusal(result));
    return undefined;
  }

  async function save(work: () => Promise<Result<SearchSave>>) {
    const saved = await attempt(work);
    if (saved === undefined) return;
    setDialog({ kind: "none" });
    if (saved.kind === "stale") setFailure(SEARCH_STALE);
    else session.showToast("Search setting saved.");
    reload();
  }

  async function model(work: () => Promise<Result<SearchView>>) {
    const done = await attempt(work);
    if (done === undefined) return;
    setDialog({ kind: "none" });
    reload();
  }

  function confirmHttp() {
    if (dialog.kind !== "http" || search.kind !== "ok" || busy) return;
    const option = dialog.option;
    const version = search.value.version;
    if (option.runs.kind !== "http") return;
    void save(() => saveSearch({ kind: "plugin", name: option.name }, hostOf(option.runs.url), version));
  }

  const actions: SearchActions = {
    onChoose: (choice) => {
      if (search.kind !== "ok" || busy) return;
      const option = choice.kind === "plugin" ? search.value.options.find((one) => one.name === choice.name) : undefined;
      if (option !== undefined && httpOption(option)) {
        setFailure("");
        setDialog({ kind: "http", option });
        return;
      }
      const version = search.value.version;
      void save(() => saveSearch(choice, null, version));
    },
    onConfirmHttp: confirmHttp,
    onDownload: () => {
      if (search.kind !== "ok") return;
      setFailure("");
      setDialog({ kind: "download", bytes: search.value.model.size_bytes });
    },
    onConfirmDownload: () => {
      if (busy || dialog.kind !== "download") return;
      const bytes = dialog.bytes;
      void model(() => downloadSearchModel(bytes));
    },
    onCancelDownload: () => { if (!busy) void model(cancelSearchModel); },
    onDelete: () => { setFailure(""); setDialog({ kind: "delete" }); },
    onConfirmDelete: () => { if (!busy) void model(deleteSearchModel); },
    onClose: () => { setDialog({ kind: "none" }); setFailure(""); },
    onRetry: reload,
  };

  return <div className="stack">
    <SearchSettingsView search={search} dialog={dialog} busy={busy} failure={failure} actions={actions} />
  </div>;
}
