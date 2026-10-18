import { useEffect, useRef, useState } from "react";
import {
  approveBaseline, baselines, connect, errorMessage, history, inspectBaseline,
  openProject, pendingIntent, projects, reconcileIntent, replaceBrief, retryIntent,
  revokeBaseline, sendMessage, status,
} from "./engine-client";
import type {
  ActionReceipt, BaselineDetail, BaselinePage, HistoryPage, IntentReceipt,
  PendingIntent, ProjectDetail, ProjectPage,
} from "./engine-client";

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
  const [view, setView] = useState<"approvals" | "conversation">("approvals");
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
    setPage((old) => old ? { ...old, fresh: false } : old);
    setDetail((old) => old ? { ...old, fresh: false } : old);
    setProjectPage((old) => old ? { ...old, fresh: false } : old);
    setProject((old) => old ? { ...old, fresh: false } : old);
    setHistoryPage((old) => old ? { ...old, fresh: false } : old);
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
    const selected = await openProject(id);
    const entries = await history();
    if (id !== activeProject.current || activeView.current !== "conversation") return;
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
    setConnected(false);
    stale();
    setDetail(undefined);
    setProject(undefined);
    setHistoryPage(undefined);
    const result = await connect();
    setConnected(result.connected);
    setPending(result.pending);
    if (activeView.current === "approvals") setPage(await baselines());
    else {
      setProjectPage(await projects());
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

  useEffect(() => { void run(reconnect); }, []);

  return {
    page, detail, connected, pending, intent, busy, error, notice, view,
    projectPage, project, historyPage, projectId, drafts,
    reconnect: () => run(reconnect),
    refresh: (next = false) => run(() => refresh(next)),
    select: (id: string) => run(async () => setDetail(await inspectBaseline(id))),
    approve: () => run(async () => { if (detail) await finishBaseline(await approveBaseline(detail.ticket)); }, true),
    revoke: () => run(async () => { if (detail) await finishBaseline(await revokeBaseline(detail.ticket)); }, true),
    reconcile: () => run(async () => finishIntent(await reconcileIntent()), true),
    retry: () => run(async () => finishIntent(await retryIntent()), true),
    showApprovals: () => {
      activeView.current = "approvals";
      setView("approvals");
      setDetail(undefined);
      return read(() => activeView.current === "approvals" ? refresh() : Promise.resolve());
    },
    showConversation: () => {
      activeView.current = "conversation";
      setView("conversation");
      setDetail((old) => old ? { ...old, fresh: false } : old);
      return read(async () => {
        if (activeView.current !== "conversation") return;
        setProjectPage(await projects());
        if (activeProject.current) await loadProject(activeProject.current);
      });
    },
    refreshProjects: (next = false) => read(async () => setProjectPage(await projects(next))),
    selectProject: (id: string) => {
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
