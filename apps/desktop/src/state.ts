import { useEffect, useRef, useState } from "react";
import {
  approveBaseline, baselines, connect, errorMessage, history, inspectBaseline,
  openProject, pendingIntent, projects, reconcileIntent, replaceBrief, retryIntent,
  revokeBaseline, sendMessage, status,
  modelAccessPage, inspectModelAccess, editModelAccess, createProject,
  startNativeAuth, readNativeAuth, cancelNativeAuth, logoutNativeAuth,
} from "./engine-client";
import type {
  ActionReceipt, BaselineDetail, BaselinePage, HistoryPage, IntentReceipt,
  PendingIntent, ProjectDetail, ProjectPage,
  ModelAccessCollection, ModelAccessPage, ModelAccessDetail,
} from "./engine-client";
import { editFromForm, newForm, formFromRecord, rebaseForm } from "./model-access";
import type { ModelAccessForm } from "./model-access";
import { creationMatches, parseProjectSpec, reduceProjectDraft } from "./screens/projects";
import type { CreationSubmission, ProjectCreationDraft } from "./screens/projects";
import { nativeAuthFailureCode, reduceNativeAuth } from "./native-auth";
import type { NativeAuthCommand, NativeAuthState } from "./native-auth";
import type { PanelAction } from "./screens/native-auth";

export interface Draft {
  text: string;
  generation: number;
  ticket: string;
  version: string;
}

export interface ProjectDrafts {
  message: Draft;
  brief: Draft;
}

function receiptMessage(receipt: ActionReceipt | IntentReceipt): string {
  switch (receipt.outcome) {
    case "applied": return "The engine recorded the decision.";
    case "rejected": return "The engine rejected this decision. No successful effect is claimed.";
    case "unavailable": return "The engine recorded execution as unavailable.";
    case "not_recorded_at_barrier": return "The intent was not recorded at the observed barrier. Delivery may still arrive; it remains pending.";
    case "unknown": return "The engine reports an unknown outcome. The exact intent remains pending.";
    case "uncertain": return "Delivery is uncertain. Reconnect and reconcile the retained exact intent.";
  }
}

export function useOwnerBrowser() {
  const [page, setPage] = useState<BaselinePage>();
  const [detail, setDetail] = useState<BaselineDetail>();
  const [projectPage, setProjectPage] = useState<ProjectPage>();
  const [project, setProject] = useState<ProjectDetail>();
  const [historyPage, setHistoryPage] = useState<HistoryPage>();
  const [projectId, setProjectId] = useState("");
  const [drafts, setDrafts] = useState<Record<string, ProjectDrafts>>(() => Object.create(null));
  const [view, setView] = useState<"approvals" | "conversation" | "settings" | "projects">("approvals");
  const [projectForm, setProjectForm] = useState<ProjectCreationDraft>({ name: "", repositories: "", generation: 0 });
  const currentProjectForm = useRef(projectForm);
  const creationSubmission = useRef<CreationSubmission | undefined>(undefined);
  const [createdProject, setCreatedProject] = useState<string>();
  const [modelCollection, setModelCollection] = useState<ModelAccessCollection>("accounts");
  const [modelPage, setModelPage] = useState<ModelAccessPage>();
  const [modelDetail, setModelDetail] = useState<ModelAccessDetail>();
  const [modelForm, setModelForm] = useState<{ value: ModelAccessForm; generation: number }>();
  const activeCollection = useRef<ModelAccessCollection>("accounts");
  const currentModelForm = useRef(modelForm);
  const selectedModel = useRef("");
  const [nativeAuth, setNativeAuth] = useState<NativeAuthState>();
  const currentNativeAuth = useRef(nativeAuth);
  const authOpening = useRef<number | undefined>(undefined);
  const [connected, setConnected] = useState(false);
  const [pending, setPending] = useState(false);
  const [intent, setIntent] = useState<PendingIntent | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const running = useRef(false);
  const queuedRead = useRef<(() => Promise<void>) | null>(null);
  const activeProject = useRef("");
  const activeView = useRef(view);
  const currentDrafts = useRef(drafts);
  const generation = useRef(0);
  const navigation = useRef(0);

  function updateNativeAuth(event: Parameters<typeof reduceNativeAuth>[1]) {
    currentNativeAuth.current = reduceNativeAuth(currentNativeAuth.current, event);
    setNativeAuth(currentNativeAuth.current);
  }

  function closeNativeAuth() {
    authOpening.current = undefined;
    updateNativeAuth({ kind: "close" });
  }

  function nextGeneration() {
    if (generation.current >= Number.MAX_SAFE_INTEGER) throw { code: "draft_generation_limit" };
    generation.current += 1;
    return generation.current;
  }

  function saveDrafts(id: string, value: ProjectDrafts) {
    if (!currentDrafts.current[id] && Object.keys(currentDrafts.current).length >= 128) {
      throw new Error("draft_capacity");
    }
    currentDrafts.current = Object.assign(Object.create(null), currentDrafts.current, { [id]: value });
    setDrafts(currentDrafts.current);
  }

  function stale() {
    updateNativeAuth({ kind: "invalidate" });
    setPage((old) => old ? { ...old, fresh: false } : old);
    setDetail((old) => old ? { ...old, fresh: false } : old);
    setProjectPage((old) => old ? { ...old, fresh: false } : old);
    setProject((old) => old ? { ...old, fresh: false } : old);
    setHistoryPage((old) => old ? { ...old, fresh: false } : old);
    setModelPage((old) => old ? { ...old, fresh: false } : old);
    setModelDetail((old) => old ? { ...old, fresh: false } : old);
  }

  function changeView(next: typeof view) {
    closeNativeAuth();
    navigation.current = nextGeneration();
    stale();
    activeView.current = next;
    setView(next);
  }

  async function loadProjects(next = false) {
    const current = navigation.current;
    const page = await projects(next);
    if (navigation.current === current && (activeView.current === "projects" || activeView.current === "conversation")) setProjectPage(page);
  }

  async function run(action: () => Promise<void>, clearNotice = false) {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setError("");
    if (clearNotice) setNotice("");
    try {
      await action();
    } catch (failure: unknown) {
      setError(errorMessage(failure));
      stale();
    } finally {
      try {
        const native = await status();
        setPending(native.pending);
        setConnected(native.connected);
        if (!native.connected) stale();
        setIntent(await pendingIntent());
      } catch {
        setConnected(false);
        stale();
      }
      running.current = false;
      setBusy(false);
      const read = queuedRead.current;
      queuedRead.current = null;
      if (read) void run(read);
    }
  }

  function read(action: () => Promise<void>) {
    if (running.current) {
      queuedRead.current = action;
      return Promise.resolve();
    }
    return run(action);
  }

  async function loadProject(id: string) {
    if (id !== activeProject.current || activeView.current !== "conversation") return;
    const current = navigation.current;
    const selected = await openProject(id);
    if (id !== activeProject.current || activeView.current !== "conversation" || current !== navigation.current) return;
    const entries = await history();
    if (id !== activeProject.current || activeView.current !== "conversation" || current !== navigation.current) return;
    setProject(selected);
    setHistoryPage(entries);
    setDetail((old) => old ? { ...old, fresh: false } : old);
    if (!currentDrafts.current[id]) {
      saveDrafts(id, {
        message: { text: "", generation: nextGeneration(), ticket: "", version: "" },
        brief: { text: selected.brief?.text ?? "", generation: nextGeneration(), ticket: selected.ticket, version: selected.brief?.version ?? "0" },
      });
    }
  }

  async function reconnect() {
    const current = navigation.current;
    setConnected(false);
    stale();
    setDetail(undefined);
    setProject(undefined);
    setHistoryPage(undefined);
    const result = await connect();
    setConnected(result.connected);
    setPending(result.pending);
    if (activeView.current === "approvals") setPage(await baselines());
    else if (activeView.current === "settings") {
      await loadModels();
      await recoverNativeAuth(current);
    }
    else {
      await loadProjects();
      if (activeProject.current) await loadProject(activeProject.current);
    }
  }

  async function refresh(next = false) {
    setDetail(undefined);
    setPage(await baselines(next));
  }

  async function finishBaseline(receipt: ActionReceipt) {
    setPending(receipt.pending);
    setNotice(`${receipt.baseline_id}: ${receiptMessage(receipt)}`);
    stale();
    if (!receipt.pending && activeView.current === "approvals") {
      setPage(await baselines());
      setDetail(await inspectBaseline(receipt.baseline_id));
    }
  }

  async function finishIntent(receipt: IntentReceipt) {
    if (receipt.kind === "create_project") {
      setPending(receipt.pending);
      setNotice(`Project declaration: ${receiptMessage(receipt)}` + (receipt.rejection ? ` ${errorMessage({ code: receipt.rejection })}` : ""));
      stale();
      const submitted = creationSubmission.current;
      if (receipt.outcome === "applied" && !receipt.pending && receipt.project_id) {
        setCreatedProject(receipt.project_id);
        if (receipt.generation === String(submitted?.generation) && creationMatches(currentProjectForm.current, submitted, navigation.current) && activeView.current === "projects") {
          retainProjectDraft({ kind: "discard", generation: nextGeneration() });
          changeView("conversation");
          activeProject.current = receipt.project_id;
          setProjectId(receipt.project_id);
          setProject(undefined);
          setHistoryPage(undefined);
          await loadProjects();
          await loadProject(receipt.project_id);
        }
      }
      if (!receipt.pending) {
        if (receipt.generation === String(submitted?.generation)) creationSubmission.current = undefined;
        if (activeView.current === "projects") await loadProjects();
      }
      return;
    }
    if (receipt.kind === "model_access") {
      setPending(receipt.pending);
      setNotice(`Model metadata: ${receiptMessage(receipt)}` + (receipt.rejection ? ` ${errorMessage({ code: receipt.rejection })}` : ""));
      stale();
      if (receipt.outcome === "applied" && !receipt.pending && String(currentModelForm.current?.generation) === receipt.generation) {
        currentModelForm.current = undefined;
        setModelForm(undefined);
      }
      if (!receipt.pending && activeView.current === "settings") await loadModels();
      return;
    }
    if (receipt.baseline_id) {
      await finishBaseline({ ...receipt, baseline_id: receipt.baseline_id });
      return;
    }
    setPending(receipt.pending);
    setNotice(`${receipt.kind} for ${receipt.project_id}: ${receiptMessage(receipt)}` + (receipt.rejection ? ` Reason: ${receipt.rejection}.` : ""));
    stale();
    const id = receipt.project_id;
    if (receipt.outcome === "applied" && !receipt.pending && id && (receipt.kind === "message" || receipt.kind === "brief")) {
      const group = currentDrafts.current[id];
      const field = receipt.kind;
      if (group && String(group[field].generation) === receipt.generation) {
        saveDrafts(id, { ...group, [field]: { ...group[field], text: field === "message" ? "" : group[field].text, generation: nextGeneration(), ticket: "" } });
      }
    }
    if (!receipt.pending && activeView.current === "conversation" && activeProject.current) await loadProject(activeProject.current);
  }

  function edit(kind: "message" | "brief", text: string) {
    const id = activeProject.current;
    if (!id) return;
    if (new TextEncoder().encode(text).length > 8192 || text.includes("\0")) {
      setError("Draft edit rejected: at most 8192 UTF-8 bytes and no NUL. The previous draft is retained.");
      return;
    }
    try {
      const group = currentDrafts.current[id] ?? {
        message: { text: "", generation: 0, ticket: "", version: "" },
        brief: { text: "", generation: 0, ticket: "", version: "" },
      };
      saveDrafts(id, { ...group, [kind]: { ...group[kind], text, generation: nextGeneration() } });
      setError("");
    } catch {
      setError("This session cannot retain another draft. Preserve or explicitly discard a project's drafts before continuing.");
    }
  }

  async function loadModels(next = false) {
    const collection = activeCollection.current;
    if (activeView.current !== "settings") return;
    updateNativeAuth({ kind: "invalidate" });
    selectedModel.current = "";
    setModelDetail(undefined);
    const page = await modelAccessPage(collection, next);
    if (activeView.current === "settings" && collection === activeCollection.current) setModelPage(page);
  }

  async function deliverNativeAuth(action: NativeAuthCommand, current: number, request: number) {
    const selected = currentNativeAuth.current;
    if (!selected?.in_flight || selected.in_flight.request !== request) return;
    try {
      const ticket = selected.context.ticket;
      const reply = action === "read" ? await readNativeAuth(ticket) :
        action === "cancel" ? await cancelNativeAuth(ticket) :
        action === "logout" ? await logoutNativeAuth(ticket) :
        await startNativeAuth(ticket, action);
      updateNativeAuth({ kind: "reply", navigation: current, request, reply });
    } catch (failure) {
      updateNativeAuth({ kind: "failure", navigation: current, request,
        code: nativeAuthFailureCode(failure) });
    }
  }

  async function recoverNativeAuth(current: number) {
    const retained = currentNativeAuth.current;
    if (!retained || !(retained.operation || retained.recover) ||
      current !== navigation.current || activeView.current !== "settings" ||
      activeCollection.current !== "connections") return;
    const basis = retained.context;
    const detail = await inspectModelAccess("connections", basis.connection);
    if (current !== navigation.current || activeView.current !== "settings" ||
      activeCollection.current !== "connections") return;
    const same = detail.config_id === basis.config_id && detail.source_hash === basis.source_hash &&
      detail.record.kind === "connection" && detail.record.connection.id === basis.connection;
    selectedModel.current = basis.connection;
    setModelDetail(detail);
    const visible = Boolean(currentNativeAuth.current?.open);
    updateNativeAuth({ kind: "open", detail: { ...detail, fresh: detail.fresh && same },
      navigation: current });
    const request = nextGeneration();
    updateNativeAuth({ kind: "begin", action: "read", request });
    if (!visible) closeNativeAuth();
    await deliverNativeAuth("read", current, request);
  }

  async function openNativeAuth() {
    if (running.current || !connected || pending || currentModelForm.current ||
      activeView.current !== "settings" || activeCollection.current !== "connections" ||
      modelDetail?.record.kind !== "connection" || !modelDetail.fresh) return;
    const basis = modelDetail;
    const id = basis.record.kind === "connection" ? basis.record.connection.id : "";
    if (selectedModel.current !== id) return;
    const current = navigation.current;
    const opening = nextGeneration();
    authOpening.current = opening;
    updateNativeAuth({ kind: "invalidate" });
    await run(async () => {
      const detail = await inspectModelAccess("connections", id);
      if (current !== navigation.current || activeView.current !== "settings" ||
        activeCollection.current !== "connections" || selectedModel.current !== id ||
        authOpening.current !== opening) return;
      authOpening.current = undefined;
      const same = detail.config_id === basis.config_id && detail.source_hash === basis.source_hash;
      setModelDetail(detail);
      updateNativeAuth({ kind: "open", detail: { ...detail, fresh: detail.fresh && same },
        navigation: current });
    });
  }

  async function nativeAuthAction(action: PanelAction) {
    if (action === "refresh") return openNativeAuth();
    if (running.current || !connected || pending || currentModelForm.current ||
      activeView.current !== "settings" || activeCollection.current !== "connections") return;
    if (action === "back" || action === "confirm_logout") {
      updateNativeAuth({ kind: action });
      return;
    }
    const current = navigation.current;
    if (currentNativeAuth.current?.context.navigation !== current) return;
    const request = nextGeneration();
    updateNativeAuth({ kind: "begin", action, request });
    if (currentNativeAuth.current?.in_flight?.request !== request) return;
    await run(() => deliverNativeAuth(action, current, request));
  }

  function retainModelForm(value: ModelAccessForm) {
    const retained = { value, generation: nextGeneration() };
    currentModelForm.current = retained;
    setModelForm(retained);
  }

  function retainProjectDraft(event: Parameters<typeof reduceProjectDraft>[1]) {
    currentProjectForm.current = reduceProjectDraft(currentProjectForm.current, event);
    setProjectForm(currentProjectForm.current);
  }

  function editProject(field: "name" | "repositories", value: string) {
    try {
      retainProjectDraft({ kind: "edit", field, value, generation: nextGeneration() });
      setError("");
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : errorMessage(failure));
    }
  }

  function discardProject() {
    try {
      retainProjectDraft({ kind: "discard", generation: nextGeneration() });
      setError("");
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : errorMessage(failure));
    }
  }

  useEffect(() => { void run(reconnect); }, []);

  return {
    page, detail, connected, pending, intent, busy, error, notice, view,
    projectPage, project, historyPage, projectId, drafts,
    projectForm, createdProject, editProject, discardProject,
    modelCollection, modelPage, modelDetail, modelForm,
    nativeAuth, openNativeAuth, nativeAuthAction, closeNativeAuth,
    reconnect: () => run(reconnect),
    refresh: (next = false) => run(() => refresh(next)),
    select: (id: string) => run(async () => setDetail(await inspectBaseline(id))),
    approve: () => run(async () => { if (detail) await finishBaseline(await approveBaseline(detail.ticket)); }, true),
    revoke: () => run(async () => { if (detail) await finishBaseline(await revokeBaseline(detail.ticket)); }, true),
    reconcile: () => run(async () => finishIntent(await reconcileIntent()), true),
    retry: () => run(async () => finishIntent(await retryIntent()), true),
    showApprovals: () => {
      changeView("approvals");
      setDetail(undefined);
      return read(() => activeView.current === "approvals" ? refresh() : Promise.resolve());
    },
    showConversation: () => {
      changeView("conversation");
      setDetail((old) => old ? { ...old, fresh: false } : old);
      return read(async () => {
        if (activeView.current !== "conversation") return;
        await loadProjects();
        if (activeProject.current) await loadProject(activeProject.current);
      });
    },
    showSettings: (collection: ModelAccessCollection = activeCollection.current) => {
      changeView("settings");
      activeCollection.current = collection;
      setModelCollection(collection);
      setModelPage(undefined);
      setModelDetail(undefined);
      return read(() => loadModels());
    },
    refreshModels: (next = false) => {
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      return read(() => loadModels(next));
    },
    selectModel: (id: string) => {
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      navigation.current = nextGeneration();
      selectedModel.current = id;
      const current = navigation.current;
      const collection = activeCollection.current;
      return read(async () => {
        const detail = await inspectModelAccess(collection, id);
        if (activeView.current === "settings" && collection === activeCollection.current &&
          current === navigation.current && selectedModel.current === id) setModelDetail(detail);
      });
    },
    createModel: () => run(async () => {
      if (currentModelForm.current || pending || activeView.current !== "settings") return;
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      const collection = activeCollection.current;
      const page = await modelAccessPage(collection);
      if (activeView.current !== "settings" || collection !== activeCollection.current) return;
      setModelPage(page);
      setModelDetail(undefined);
      if (page.writable) retainModelForm(newForm(collection, page.ticket));
    }),
    editModel: (remove = false) => {
      if (!modelDetail?.fresh || !modelDetail.writable || currentModelForm.current || pending) return;
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      try { retainModelForm(formFromRecord(modelDetail.record, modelDetail.ticket, remove)); }
      catch { setError("The metadata form could not be retained. Preserve its values before restarting."); }
    },
    changeModelForm: (patch: Partial<ModelAccessForm>) => {
      const retained = currentModelForm.current;
      if (!retained) return;
      const value = { ...retained.value, ...patch };
      if (JSON.stringify(value).length > 32768 || Object.values(value).some((value) => typeof value === "string" && (value.length > 16384 || value.includes("\0")))) {
        setError("Metadata edit exceeds the form bounds or contains NUL. Previous values are retained.");
        return;
      }
      try { retainModelForm(value); setError(""); }
      catch { setError("This draft reached its safe generation limit. Preserve its values before restarting."); }
    },
    reviewModelForm: () => run(async () => {
      const retained = currentModelForm.current;
      if (!retained || pending) return;
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      const form = retained.value;
      activeCollection.current = form.collection;
      setModelCollection(form.collection);
      const basis = form.action === "create" ? await modelAccessPage(form.collection) : await inspectModelAccess(form.collection, form.id);
      if (form.action === "create") { setModelPage(basis as ModelAccessPage); setModelDetail(undefined); }
      else setModelDetail(basis as ModelAccessDetail);
      if (!basis.writable) throw { code: "settings_read_only" };
      if (currentModelForm.current === retained) retainModelForm(rebaseForm(form, basis));
      setNotice("Latest owner metadata loaded. Review the retained values before explicitly saving.");
    }),
    discardModelForm: () => { currentModelForm.current = undefined; setModelForm(undefined); },
    submitModel: () => run(async () => {
      const retained = currentModelForm.current;
      if (!retained || pending || activeView.current !== "settings") return;
      let edit;
      try { edit = editFromForm(retained.value); }
      catch (failure) { setError(failure instanceof Error ? failure.message : "Invalid metadata form."); return; }
      await finishIntent(await editModelAccess(retained.value.ticket, edit, retained.generation));
    }, true),
    showProjects: () => {
      changeView("projects");
      return read(() => activeView.current === "projects" ? loadProjects() : Promise.resolve());
    },
    submitProject: () => run(async () => {
      if (pending || !connected || !projectPage?.fresh || activeView.current !== "projects") return;
      const draft = currentProjectForm.current;
      let spec;
      try { spec = parseProjectSpec(draft.name, draft.repositories); }
      catch (failure) { setError(failure instanceof Error ? failure.message : "Invalid project declaration."); return; }
      creationSubmission.current = { generation: draft.generation, navigation: navigation.current };
      await finishIntent(await createProject(spec, draft.generation));
    }, true),
    refreshProjects: (next = false) => read(() => loadProjects(next)),
    selectProject: (id: string) => {
      changeView("conversation");
      activeProject.current = id;
      setProjectId(id);
      setProject(undefined);
      setHistoryPage(undefined);
      return read(() => loadProject(id));
    },
    refreshProject: () => read(() => loadProject(activeProject.current)),
    refreshHistory: (next = false) => read(async () => {
      const id = activeProject.current;
      const entries = await history(next);
      if (entries.project_id === id && id === activeProject.current) setHistoryPage(entries);
    }),
    edit,
    loadBrief: () => {
      if (!project?.fresh || project.project_id !== activeProject.current) return;
      try {
        const group = currentDrafts.current[project.project_id];
        if (group) saveDrafts(project.project_id, { ...group, brief: { text: project.brief?.text ?? "", generation: nextGeneration(), ticket: project.ticket, version: project.brief?.version ?? "0" } });
      } catch { setError("The draft could not be retained. Preserve its text before restarting."); }
    },
    discardDrafts: () => {
      const id = activeProject.current;
      const copy: Record<string, ProjectDrafts> = Object.assign(Object.create(null), currentDrafts.current);
      delete copy[id];
      currentDrafts.current = copy;
      setDrafts(copy);
    },
    submit: (kind: "message" | "brief") => run(async () => {
      const id = activeProject.current;
      const draft = currentDrafts.current[id]?.[kind];
      if (!draft || !project?.fresh || project.project_id !== id || pending) return;
      const ticket = kind === "brief" ? draft.ticket : project.ticket;
      await finishIntent(await (kind === "brief" ? replaceBrief : sendMessage)(ticket, draft.text, draft.generation));
    }, true),
  };
}
