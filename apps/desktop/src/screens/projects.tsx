import type { ProjectSpec } from "../engine-client";
import type { useOwnerBrowser } from "../state";
import { ProjectList } from "./conversation";

export interface ProjectCreationDraft {
  name: string;
  repositories: string;
  generation: number;
}

export interface CreationSubmission {
  generation: number;
  navigation: number;
}

type ProjectDraftEvent =
  | { kind: "edit"; field: "name" | "repositories"; value: string; generation: number }
  | { kind: "discard"; generation: number };

export function parseProjectSpec(name: string, paths: string): ProjectSpec {
  const bytes = (value: string) => new TextEncoder().encode(value).length;
  const controls = /[\u0000-\u001f\u007f-\u009f]/u;
  if (!name.trim() || bytes(name) > 256 || controls.test(name)) {
    throw new Error("Enter a nonblank name within 256 UTF-8 bytes, without control characters.");
  }
  if (bytes(paths) > 65536) throw new Error("Repository input exceeds 65536 UTF-8 bytes.");
  const repositories = paths.split(/\r?\n/u);
  if (repositories.length < 1 || repositories.length > 64 || repositories.some((path) =>
    !path.startsWith("/") || bytes(path) > 4096 || controls.test(path) || path.includes("://"))) {
    throw new Error("Enter 1 to 64 absolute local paths, one per line, within 4096 UTF-8 bytes each, without blank lines, URLs or controls.");
  }
  const spec = { name, repositories };
  if (bytes(JSON.stringify(spec)) > 65536) throw new Error("The declaration exceeds the engine frame bound. Reduce the entered paths.");
  return spec;
}

export function reduceProjectDraft(draft: ProjectCreationDraft, event: ProjectDraftEvent): ProjectCreationDraft {
  if (!Number.isSafeInteger(event.generation) || event.generation <= draft.generation) {
    throw new Error("The creation draft requires a new safe generation.");
  }
  if (event.kind === "discard") return { name: "", repositories: "", generation: event.generation };
  const limit = event.field === "name" ? 256 : 65536;
  if (new TextEncoder().encode(event.value).length > limit || event.value.includes("\0")) {
    throw new Error(`This edit exceeds ${limit} UTF-8 bytes or contains NUL. The previous draft is retained.`);
  }
  return { ...draft, [event.field]: event.value, generation: event.generation };
}

export function creationMatches(draft: ProjectCreationDraft, submitted: CreationSubmission | undefined, navigation: number): boolean {
  return submitted !== undefined && draft.generation === submitted.generation && navigation === submitted.navigation;
}

export function Projects({ state }: { state: ReturnType<typeof useOwnerBrowser> }) {
  let spec: ProjectSpec | undefined;
  let validation = "";
  try { spec = parseProjectSpec(state.projectForm.name, state.projectForm.repositories); }
  catch (failure) { validation = failure instanceof Error ? failure.message : "Invalid project declaration."; }
  const blocked = state.busy || state.pending || !state.connected || !state.projectPage?.fresh;
  return <section className="baseline-browser" aria-busy={state.busy}>
    <ProjectList state={state} />
    <div className="inspection">
      <h2>Create a project</h2>
      <p>Declaring a project does not start or run work. Paths are declarations; repositories are not inspected or admitted.</p>
      <form aria-label="Create project" onSubmit={(event) => { event.preventDefault(); void state.submitProject(); }}>
        <div className="conversation-editor">
          <label htmlFor="project-name">Project name</label>
          <input id="project-name" maxLength={256} value={state.projectForm.name} onChange={(event) => state.editProject("name", event.target.value)} aria-describedby="project-name-bound" />
          <p id="project-name-bound" className="muted">At most 256 UTF-8 bytes; no control characters.</p>
          <label htmlFor="project-repositories">Declared local repositories</label>
          <textarea id="project-repositories" maxLength={65536} rows={6} value={state.projectForm.repositories} onChange={(event) => state.editProject("repositories", event.target.value)} aria-describedby="project-path-bound" />
          <p id="project-path-bound" className="muted">One absolute local path per line, 1 to 64 paths, at most 4096 UTF-8 bytes per path. Values and order are retained exactly.</p>
          <p className="muted">{spec ? `${spec.repositories.length} declared repositories` : validation}</p>
          <div className="button-row">
            <button type="submit" disabled={blocked || !spec}>Create project</button>
            <button type="button" onClick={state.discardProject} disabled={!state.projectForm.name && !state.projectForm.repositories}>Discard creation draft</button>
          </div>
        </div>
      </form>
      {state.createdProject && <p role="status">Created project: <button disabled={!state.connected} onClick={() => void state.selectProject(state.createdProject!)}>{state.createdProject}</button></p>}
      <p className="muted">Creation drafts are independent of conversation drafts. Discarding a draft keeps any exact native intent available for reconciliation. Open a project to record its first brief explicitly.</p>
    </div>
  </section>;
}
