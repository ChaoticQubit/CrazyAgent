import { useEffect, useRef, useState } from "react";
import {
  approveBaseline, baselines, connect, errorMessage, history, inspectBaseline,
  openProject, pendingIntent, projects, reconcileIntent, replaceBrief, retryIntent,
  revokeBaseline, sendMessage, status,
  modelAccessPage, inspectModelAccess, editModelAccess, createProject,
  startNativeAuth, readNativeAuth, cancelNativeAuth, logoutNativeAuth, submitClaudeAuthCode,
  prepareManagedAccount, prepareConfiguredAccount, accountAuthCapabilities,
  configureAccountProfile, readAccountProfile,
} from "./engine-client";
import type {
  ActionReceipt, BaselineDetail, BaselinePage, HistoryPage, IntentReceipt,
  PendingIntent, ProjectDetail, ProjectPage,
  ModelAccessCollection, ModelAccessPage, ModelAccessDetail,
  ManagedAccountPlan, ConfiguredAccountPlan, AccountAuthCapabilitiesReply, AccountProfileView,
  ModelAccessRecord, ModelAccessEdit, AccountPool,
} from "./engine-client";
import { editFromForm, newForm, formFromRecord, rebaseForm, managedAccountInput, configuredAccountInput, setPoolConnection, withPoolProject } from "./model-access";
import type { ModelAccessForm } from "./model-access";
import { creationMatches, parseProjectSpec, reduceProjectDraft } from "./screens/projects";
import type { CreationSubmission, ProjectCreationDraft } from "./screens/projects";
import { nativeAuthFailureCode, reduceNativeAuth, nativeClaudeInputAllowed, validateClaudeAuthCode } from "./native-auth";
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

interface ManagedSetup {
  plan: ManagedAccountPlan | ConfiguredAccountPlan;
  next: number;
  waiting?: number;
  verifying?: boolean;
  message: string;
}

interface PoolAssignment {
  pool: AccountPool;
  projectId: string;
  generation: number;
  verifying: boolean;
}

interface AccountProfileSetup {
  detail: ModelAccessDetail;
  capabilities: AccountAuthCapabilitiesReply;
  navigation: number;
  generation?: number;
  result?: AccountProfileView;
  uncertain: boolean;
  submitting: boolean;
  message: string;
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
  const [poolConnections, setPoolConnections] = useState<ModelAccessPage>();
  const [poolProjectId, setPoolProjectId] = useState("");
  const [poolAssignment, setPoolAssignment] = useState<PoolAssignment>();
  const currentPoolAssignment = useRef(poolAssignment);
  const [modelForm, setModelForm] = useState<{ value: ModelAccessForm; generation: number }>();
  const activeCollection = useRef<ModelAccessCollection>("accounts");
  const currentModelForm = useRef(modelForm);
  const [accountDraft, setAccountDraft] = useState({ provider: "codex", label: "", url: "", model: "" });
  const currentAccountDraft = useRef(accountDraft);
  const [managedSetup, setManagedSetup] = useState<ManagedSetup>();
  const currentManagedSetup = useRef(managedSetup);
  const [accountProfile, setAccountProfile] = useState<AccountProfileSetup>();
  const currentAccountProfile = useRef(accountProfile);
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
    setPoolConnections((old) => old ? { ...old, fresh: false } : old);
  }

  function changeView(next: typeof view) {
    closeNativeAuth();
    if (!currentAccountProfile.current?.uncertain && !currentAccountProfile.current?.submitting) retainAccountProfile(undefined);
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
        setPending(native.pending || Boolean(currentAccountProfile.current?.uncertain || currentAccountProfile.current?.submitting));
        setConnected(native.connected);
        if (!native.connected) stale();
        const retainedIntent = await pendingIntent();
        setIntent(retainedIntent);
        const setup = currentManagedSetup.current;
        if (!native.pending && !retainedIntent && setup?.waiting !== undefined) {
          retainManagedSetup({ ...setup, waiting: undefined,
            message: "The native bridge has no pending request for this step. Review current owner state before explicitly continuing." });
        }
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
    setPending(result.pending || Boolean(currentAccountProfile.current?.uncertain || currentAccountProfile.current?.submitting));
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
      const assignment = currentPoolAssignment.current;
      if (assignment && String(assignment.generation) === receipt.generation) {
        setPending(receipt.pending);
        stale();
        setNotice(`Pool assignment: ${receiptMessage(receipt)} Exact readback is required before use.`);
        if (receipt.outcome === "applied" && !receipt.pending) {
          retainPoolAssignment({ ...assignment, verifying: true });
          await verifyPoolAssignment();
        } else if (!receipt.pending) retainPoolAssignment(undefined);
        return;
      }
      const setup = currentManagedSetup.current;
      if (setup?.waiting !== undefined && String(setup.waiting) === receipt.generation) {
        setPending(receipt.pending);
        stale();
        retainManagedSetup({ ...setup, message: receiptMessage(receipt) });
        if (receipt.outcome === "applied" && !receipt.pending) {
          retainManagedSetup({ ...setup, waiting: undefined, verifying: true, message: "The owner recorded this step. Verifying its exact saved record." });
          await verifyManagedStep();
        } else if (!receipt.pending) {
          retainManagedSetup({ ...setup, waiting: undefined, message: `${receiptMessage(receipt)} Review current owner state before continuing.` });
        }
        return;
      }
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
    setPoolConnections(undefined);
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

  async function submitNativeClaudeCode(code: string) {
    try {
      validateClaudeAuthCode(code);
      if (running.current || !connected || pending || currentModelForm.current ||
        activeView.current !== "settings" || activeCollection.current !== "connections" ||
        !nativeClaudeInputAllowed(currentNativeAuth.current, false)) return;
      const selected = currentNativeAuth.current;
      if (!selected || selected.context.navigation !== navigation.current) return;
      const current = navigation.current;
      const ticket = selected.context.ticket;
      const request = nextGeneration();
      updateNativeAuth({ kind: "claude_input_begin", request });
      if (currentNativeAuth.current?.claude_input?.request !== request) return;
      await run(async () => {
        try {
          const delivery = submitClaudeAuthCode(ticket, code);
          code = "";
          await delivery;
          updateNativeAuth({ kind: "claude_input_accepted", navigation: current, request });
        } catch {
          updateNativeAuth({ kind: "claude_input_failure", navigation: current, request });
        } finally {
          code = "";
        }
      });
    } finally {
      code = "";
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

  function retainPoolAssignment(value: PoolAssignment | undefined) {
    currentPoolAssignment.current = value;
    setPoolAssignment(value);
  }

  async function verifyPoolAssignment() {
    const assignment = currentPoolAssignment.current;
    if (!assignment) return;
    const detail = await inspectModelAccess("pools", assignment.pool.id);
    if (currentPoolAssignment.current !== assignment) return;
    if (!detail.fresh || !detail.writable || detail.record.kind !== "pool" ||
      JSON.stringify(detail.record.pool) !== JSON.stringify(assignment.pool)) {
      throw new Error("The exact saved pool assignment is not verified. Refresh and reconcile the retained intent before using it.");
    }
    retainPoolAssignment(undefined);
    if (currentModelForm.current?.generation === assignment.generation) {
      currentModelForm.current = undefined;
      setModelForm(undefined);
    }
    if (activeView.current === "settings" && activeCollection.current === "pools") {
      setModelDetail(detail);
      selectedModel.current = assignment.pool.id;
    }
    setNotice(`Saved and verified pool ${assignment.pool.id} for project ${assignment.projectId} and project_leader. Authentication and execution readiness are separate.`);
  }

  function retainManagedSetup(value: ManagedSetup | undefined) {
    currentManagedSetup.current = value;
    setManagedSetup(value);
  }

  function retainAccountProfile(value: AccountProfileSetup | undefined) {
    currentAccountProfile.current = value;
    setAccountProfile(value);
  }

  function acceptAccountProfile(context: AccountProfileSetup, reply: AccountProfileView) {
    if (currentAccountProfile.current !== context) throw new Error("The private operation context changed. Read the original operation before continuing.");
    if (reply.ticket !== context.detail.ticket || reply.generation !== String(context.generation) ||
      reply.reply.database_instance !== context.capabilities.database_instance || reply.reply.engine_epoch !== context.capabilities.engine_epoch ||
      (context.result && context.result.reply.request_id !== reply.reply.request_id)) {
      throw new Error("The private operation identity changed. Its outcome remains uncertain.");
    }
    if (reply.reply.outcome !== "not_recorded") {
      const receipt = reply.reply.outcome.configured_only.receipt;
      if (receipt.config_id !== context.detail.config_id || receipt.source_hash !== context.detail.source_hash ||
        context.detail.record.kind !== "connection" || JSON.stringify(receipt.connection) !== JSON.stringify(context.detail.record.connection)) {
        throw new Error("Private profile readback does not match the exact saved connection. No readiness is claimed.");
      }
    }
    retainAccountProfile({ ...context, result: reply, uncertain: reply.pending, submitting: false,
      message: reply.reply.outcome === "not_recorded" ? (reply.pending ?
        "Not recorded is not a cancellation barrier. Keep reading the original operation; no key will be resent." :
        "The owner closed the original operation without recording a profile. Explicitly read the current capability before a new submission.") :
        "Configured only: the owner verified this exact private profile. Authentication, model readiness and engine admission remain separate." });
  }

  async function submitAccountProfile(secret: string | undefined) {
    const context = currentAccountProfile.current;
    if (!context || context.uncertain || context.submitting || context.result || context.navigation !== navigation.current ||
      activeView.current !== "settings" || !connected || pending || context.capabilities.configured !== "available" ||
      context.detail.record.kind !== "connection" || !modelDetail?.fresh || modelDetail.ticket !== context.detail.ticket) return;
    const usesKey = context.detail.record.connection.auth.method === "api_key";
    if (usesKey !== (secret !== undefined)) throw new Error("The private entry method does not match the saved connection.");
    const submitted = { ...context, generation: nextGeneration(), uncertain: true, submitting: true,
      message: "Sending one private profile request. The key is not retained or automatically replayed." };
    retainAccountProfile(submitted);
    try {
      const reply = await configureAccountProfile(context.detail.ticket, secret, submitted.generation);
      acceptAccountProfile(submitted, reply);
    } catch {
      if (currentAccountProfile.current === submitted) retainAccountProfile({ ...submitted, submitting: false,
        message: "The private submission did not return a verified result. Read its original operation before any new submission. The key was not retained." });
    }
  }

  function managedStep(setup: ManagedSetup): { collection: ModelAccessCollection; record: ModelAccessRecord; edit: ModelAccessEdit; id: string } {
    const { connection, pool } = setup.plan;
    if (setup.next === 0) return { collection: "accounts", id: connection.account_ref, record: { kind: "account", account_ref: connection.account_ref }, edit: { kind: "add_account", account_ref: connection.account_ref } };
    if (setup.next === 1) return { collection: "connections", id: connection.id, record: { kind: "connection", connection }, edit: { kind: "create_connection", connection } };
    if (setup.next === 2) return { collection: "pools", id: pool.id, record: { kind: "pool", pool }, edit: { kind: "create_pool", pool } };
    throw new Error("Account setup already verified all three configuration records.");
  }

  async function verifyManagedStep() {
    const setup = currentManagedSetup.current;
    if (!setup?.verifying) return;
    const step = managedStep(setup);
    const detail = await inspectModelAccess(step.collection, step.id);
    if (currentManagedSetup.current !== setup) return;
    if (!detail.fresh || !detail.writable || JSON.stringify(detail.record) !== JSON.stringify(step.record)) {
      throw new Error("The saved account record could not be verified exactly. The original setup remains retained.");
    }
    retainManagedSetup({ ...setup, next: setup.next + 1, verifying: false,
      message: setup.next === 2 ? "Account, connection and pool are configured. Sign-in or private profile setup is a separate owner action." : "This exact configuration record was verified. Continue with the remaining saved identities." });
  }

  async function inspectManagedAccount() {
    const setup = currentManagedSetup.current;
    if (!setup || setup.next !== 3 || activeView.current !== "settings" || currentAccountProfile.current?.uncertain) return;
    closeNativeAuth();
    activeCollection.current = "connections";
    setModelCollection("connections");
    setModelPage(undefined);
    const current = navigation.current;
    const detail = await inspectModelAccess("connections", setup.plan.connection.id);
    if (currentManagedSetup.current !== setup || current !== navigation.current || activeView.current !== "settings") return;
    if (!detail.fresh || !detail.writable || detail.record.kind !== "connection" || JSON.stringify(detail.record.connection) !== JSON.stringify(setup.plan.connection)) {
      throw new Error("The configured connection changed. Inspect its current record before sign-in.");
    }
    selectedModel.current = setup.plan.connection.id;
    setModelDetail(detail);
    if (setup.plan.provider === "codex" || setup.plan.provider === "claude") {
      updateNativeAuth({ kind: "open", detail, navigation: current });
    } else {
      const capabilities = await accountAuthCapabilities(detail.ticket);
      if (currentManagedSetup.current !== setup || current !== navigation.current || activeView.current !== "settings") return;
      if (capabilities.config_id !== detail.config_id || capabilities.source_hash !== detail.source_hash ||
        JSON.stringify(capabilities.connection) !== JSON.stringify(setup.plan.connection)) throw new Error("Private setup capability does not match the saved connection. Refresh before proceeding.");
      retainAccountProfile({ detail, capabilities, navigation: current, uncertain: false, submitting: false,
        message: capabilities.configured === "available" ? "The owner permits private configuration for this exact saved connection. Confirm the separate action below." :
          "The owner reports this private producer unavailable. Endpoint configuration is saved; no authentication or readiness is claimed." });
    }
  }

  async function continueManagedAccount() {
    const current = navigation.current;
    if (activeView.current !== "settings" || currentModelForm.current) return;
    closeNativeAuth();
    if (currentManagedSetup.current?.verifying) await verifyManagedStep();
    for (let stepIndex = 0; stepIndex < 3; stepIndex += 1) {
      const setup = currentManagedSetup.current;
      if (!setup || setup.waiting !== undefined || setup.verifying || setup.next === 3 || current !== navigation.current || activeView.current !== "settings") break;
      const step = managedStep(setup);
      const page = await modelAccessPage(step.collection);
      if (currentManagedSetup.current !== setup || current !== navigation.current || activeView.current !== "settings") return;
      if (!page.fresh || !page.writable) throw { code: "settings_read_only" };
      activeCollection.current = step.collection;
      setModelCollection(step.collection);
      setModelPage(page);
      setModelDetail(undefined);
      const waiting = nextGeneration();
      retainManagedSetup({ ...setup, waiting, message: "Saving one configuration record through the authenticated owner." });
      await finishIntent(await editModelAccess(page.ticket, step.edit, waiting));
      if (currentManagedSetup.current?.next === setup.next) break;
    }
    if (current === navigation.current) await inspectManagedAccount();
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
    nextGeneration,
    page, detail, connected, pending, intent, busy, error, notice, view,
    projectPage, project, historyPage, projectId, drafts,
    projectForm, createdProject, editProject, discardProject,
    modelCollection, modelPage, modelDetail, modelForm,
    poolConnections, poolProjectId, poolAssignment,
    loadPoolProjects: (next = false) => run(async () => {
      const retained = currentModelForm.current;
      if (!connected || pending || activeView.current !== "settings" || activeCollection.current !== "pools" ||
        !retained || retained.value.collection !== "pools" || retained.value.action !== "replace") return;
      if (next && (!projectPage?.fresh || !projectPage.has_more)) throw { code: "no_next_page" };
      const current = navigation.current;
      stale();
      const choices = await projects(next);
      if (currentModelForm.current !== retained || current !== navigation.current ||
        activeView.current !== "settings" || activeCollection.current !== "pools") return;
      setProjectPage(choices);
      setPoolProjectId("");
      setNotice("Choose an actual project, then explicitly confirm saving this pool for its project_leader role.");
    }),
    changePoolProject: (id: string) => {
      if (running.current || pending || !connected || !projectPage?.fresh ||
        activeView.current !== "settings" || activeCollection.current !== "pools" ||
        !projectPage.items.some((row) => row.id === id)) return;
      setPoolProjectId(id);
    },
    confirmPoolProject: () => run(async () => {
      const retained = currentModelForm.current;
      const original = modelDetail;
      if (!connected || pending || currentPoolAssignment.current || !retained ||
        retained.value.collection !== "pools" || retained.value.action !== "replace" ||
        original?.record.kind !== "pool" || original.record.pool.id !== retained.value.id ||
        !projectPage?.fresh || !projectPage.items.some((row) => row.id === poolProjectId) ||
        activeView.current !== "settings" || activeCollection.current !== "pools") return;
      const current = navigation.current;
      const chosen = poolProjectId;
      stale();
      const opened = await openProject(chosen);
      if (current !== navigation.current || currentModelForm.current !== retained || activeView.current !== "settings") return;
      if (!opened.fresh || opened.project_id !== chosen) throw { code: "invalid_project" };
      const basis = await inspectModelAccess("pools", retained.value.id);
      if (current !== navigation.current || currentModelForm.current !== retained || activeView.current !== "settings") return;
      setModelDetail(basis);
      if (!basis.fresh || !basis.writable || JSON.stringify(basis.record) !== JSON.stringify(original.record)) {
        throw new Error("The saved pool changed. Refresh and review the retained draft before confirming its assignment.");
      }
      const value = withPoolProject(rebaseForm(retained.value, basis), opened);
      const edit = editFromForm(value);
      if (edit.kind !== "replace_pool") throw new Error("Project assignment requires an existing pool replacement.");
      retainModelForm(value);
      const generation = currentModelForm.current!.generation;
      retainPoolAssignment({ pool: edit.pool, projectId: chosen, generation, verifying: false });
      await finishIntent(await editModelAccess(value.ticket, edit, generation));
    }, true),
    verifyPoolAssignment: () => run(async () => {
      if (connected && !pending) await verifyPoolAssignment();
    }),
    loadPoolConnections: (next = false) => run(async () => {
      const retained = currentModelForm.current;
      if (!connected || pending || activeView.current !== "settings" || activeCollection.current !== "pools" ||
        !retained || retained.value.collection !== "pools" || retained.value.action === "remove") return;
      if (next && (!poolConnections?.fresh || !poolConnections.has_more)) throw { code: "no_next_page" };
      const current = navigation.current;
      closeNativeAuth();
      stale();
      const choices = await modelAccessPage("connections", next);
      if (currentModelForm.current !== retained || current !== navigation.current ||
        activeView.current !== "settings" || activeCollection.current !== "pools") return;
      setPoolConnections(choices);
      setNotice("Saved connections loaded. Choose members, then Refresh and review retained form before saving the pool.");
    }),
    changePoolConnection: (id: string, selected: boolean) => {
      const retained = currentModelForm.current;
      if (running.current || pending || !connected || activeView.current !== "settings" ||
        activeCollection.current !== "pools" || !retained || !poolConnections?.fresh ||
        !poolConnections.items.some((record) => record.kind === "connection" && record.connection.id === id)) return;
      try { retainModelForm(setPoolConnection(retained.value, id, selected)); setError(""); }
      catch (failure) { setError(failure instanceof Error ? failure.message : "The pool selection could not be retained."); }
    },
    accountDraft, managedSetup, accountProfile,
    submitAccountSecret: (secret: string) => run(() => submitAccountProfile(secret)),
    configureLocalAccount: () => run(() => submitAccountProfile(undefined)),
    reconcileAccountProfile: () => run(async () => {
      const context = currentAccountProfile.current;
      if (!context?.uncertain || context.submitting || !connected) return;
      try { acceptAccountProfile(context, await readAccountProfile()); }
      catch {
        if (currentAccountProfile.current === context) retainAccountProfile({ ...context,
          message: "The original private operation remains unresolved. No key was retained or resent. Restore its owner connection and read again." });
      }
    }),
    changeAccountDraft: (patch: Partial<typeof accountDraft>) => {
      if (currentManagedSetup.current || currentAccountProfile.current?.uncertain || running.current) return;
      const value = { ...currentAccountDraft.current, ...patch };
      if (!["codex", "claude", "api_key", "local"].includes(value.provider) ||
        new TextEncoder().encode(value.label).length > 96 || value.url.length > 2048 || value.model.length > 256 ||
        Object.values(value).some((field) => /[\u0000-\u001f\u007f-\u009f]/u.test(field))) {
        setError("Account draft exceeds its input bounds. Previous values are retained.");
        return;
      }
      currentAccountDraft.current = value;
      setAccountDraft(value);
      setError("");
    },
    submitManagedAccount: () => run(async () => {
      if (!connected || pending || currentManagedSetup.current || currentAccountProfile.current?.uncertain || currentModelForm.current || currentPoolAssignment.current || activeView.current !== "settings") return;
      const draft = currentAccountDraft.current;
      const current = navigation.current;
      let plan: ManagedAccountPlan | ConfiguredAccountPlan;
      if (draft.provider === "codex" || draft.provider === "claude") {
        const input = managedAccountInput(draft.provider, draft.label);
        plan = await prepareManagedAccount(input.provider, input.label);
      } else {
        const input = configuredAccountInput(draft.provider, draft.label, draft.url, draft.model);
        plan = await prepareConfiguredAccount(input.provider, input.label, input.url, input.model);
      }
      retainAccountProfile(undefined);
      retainManagedSetup({ plan, next: 0, message: "Native-generated configuration identities retained. No sign-in or private submission has started." });
      if (current === navigation.current && activeView.current === "settings") await continueManagedAccount();
    }, true),
    continueManagedAccount: () => run(async () => { if (connected && !pending) await continueManagedAccount(); }, true),
    inspectManagedAccount: () => run(async () => { if (connected && !pending) await inspectManagedAccount(); }),
    closeManagedAccount: () => {
      if (running.current || pending || currentAccountProfile.current?.uncertain || currentManagedSetup.current?.waiting !== undefined) return;
      retainManagedSetup(undefined);
      retainAccountProfile(undefined);
      setNotice("Account setup closed. Any saved account, connection and pool remain available in settings.");
    },
    nativeAuth, openNativeAuth, nativeAuthAction, closeNativeAuth, submitNativeClaudeCode,
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
      if (currentModelForm.current || currentPoolAssignment.current || pending || activeView.current !== "settings") return;
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      const collection = activeCollection.current;
      const page = await modelAccessPage(collection);
      if (activeView.current !== "settings" || collection !== activeCollection.current) return;
      setModelPage(page);
      setModelDetail(undefined);
      setPoolConnections(undefined);
      if (page.writable) retainModelForm(newForm(collection, page.ticket));
    }),
    editModel: (remove = false) => {
      if (!modelDetail?.fresh || !modelDetail.writable || currentModelForm.current || currentPoolAssignment.current || pending) return;
      closeNativeAuth();
      updateNativeAuth({ kind: "invalidate" });
      setPoolConnections(undefined);
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
      setPoolConnections((old) => old ? { ...old, fresh: false } : old);
      if (form.action === "create") { setModelPage(basis as ModelAccessPage); setModelDetail(undefined); }
      else setModelDetail(basis as ModelAccessDetail);
      if (!basis.writable) throw { code: "settings_read_only" };
      if (currentModelForm.current === retained) retainModelForm(rebaseForm(form, basis));
      setNotice("Latest owner metadata loaded. Review the retained values before explicitly saving.");
    }),
    discardModelForm: () => {
      if (pending || running.current || (currentPoolAssignment.current && !connected)) return;
      if (currentPoolAssignment.current) {
        retainPoolAssignment(undefined);
        setNotice("Local assignment draft discarded. Inspect the current saved pool before use; no binding success is claimed.");
      }
      currentModelForm.current = undefined; setModelForm(undefined); setPoolConnections(undefined); setPoolProjectId("");
    },
    submitModel: () => run(async () => {
      const retained = currentModelForm.current;
      if (!retained || pending || currentPoolAssignment.current || activeView.current !== "settings") return;
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
      return read(async () => {
        await loadProjects();
        await loadProject(id);
      });
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
