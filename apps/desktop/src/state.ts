import { createContext, useContext, useEffect, useRef, useState } from "react";
import * as api from "./api";
import type { OwnerProfile, Result } from "./api";
import type { EngineState } from "./ui/shell";
import type { Route } from "./ui/router";
import { watchUnconfirmed, leavesProfileUnconfirmed } from "./engine-client";
import type { ActionReceipt, IntentReceipt, PendingIntent, ProjectPage } from "./engine-client";
import { INPUT_DEFAULT, inputChoice } from "./screens/team/terminal-io";
import type { InputChoice } from "./screens/team/terminal-io";
export type Loaded<T> = { kind: "loading" } | Result<T>;
export type ThemeChoice = "system" | "light" | "dark";
export type ProjectRow = ProjectPage["items"][number];

const THEME_KEY = "crazyagent.theme";
const MASTER_INPUT_KEY = "crazyagent.master-input";

/** Reads where the owner types to a live master, as kept in this app (not in the engine).
 * Output: the choice; the default when storage is unavailable or holds something else. */
export function readMasterInput(): InputChoice {
  try {
    return inputChoice(window.localStorage.getItem(MASTER_INPUT_KEY));
  } catch {
    return INPUT_DEFAULT;
  }
}

/** Remembers where the owner types to a live master. Input: the choice. Output: none. */
export function saveMasterInput(choice: InputChoice): void {
  try {
    window.localStorage.setItem(MASTER_INPUT_KEY, choice);
  } catch {
    return;
  }
}

/** Reads the owner's appearance choice kept in this app (not in the engine profile).
 * Output: system, light or dark; system when storage is unavailable. */
export function readTheme(): ThemeChoice {
  try {
    const value = window.localStorage.getItem(THEME_KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

/** Applies and remembers the appearance choice; system follows the macOS setting.
 * Input: the choice. Output: none. */
export function applyTheme(choice: ThemeChoice): void {
  if (typeof document === "undefined") return;
  if (choice === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", choice);
  try {
    if (choice === "system") window.localStorage.removeItem(THEME_KEY);
    else window.localStorage.setItem(THEME_KEY, choice);
  } catch {
    return;
  }
}

/** Reads the engine state from a connect or status reply.
 * Input: the reply. Output: running when connected, outdated when the engine is another
 * version than this app, otherwise down. */
export function engineState(result: Result<api.ConnectionStatus>): EngineState {
  if (result.kind === "error" && result.code === "engine_version_mismatch") return "outdated";
  return result.kind === "ok" && result.value.connected ? "running" : "down";
}

/** The refusal codes after which the session reads again whether a sign-in is stuck. */
export const STUCK_SIGN_IN_CODES = ["native_auth_recovery_required", "native_auth_outcome_unknown_read_same_operation"];

/** The change the bridge still keeps unconfirmed, the last check's outcome, and whether a
 * check or retry is running. */
export interface Unconfirmed {
  intent: PendingIntent;
  outcome: ActionReceipt["outcome"] | null;
  busy: boolean;
}

export interface Session {
  engine: EngineState;
  unconfirmed: Unconfirmed | null;
  profileCheck: { busy: boolean } | null;
  stuckSignIn: { busy: boolean } | null;
  profile: Loaded<OwnerProfile | null>;
  projects: Loaded<ProjectRow[]>;
  projectsMore: boolean;
  toast: string;
  toastOpen: Route | null;
  revision: number;
  retry: () => void;
  checkUnconfirmed: () => Promise<void>;
  checkProfile: () => Promise<void>;
  clearStuckSignIn: () => Promise<void>;
  retryUnconfirmed: () => Promise<void>;
  dropUnconfirmed: () => Promise<void>;
  refreshProfile: () => Promise<void>;
  refreshProjects: () => Promise<void>;
  showToast: (message: string, open?: Route) => void;
  noteResult: <T>(result: Result<T>) => Result<T>;
}

/** The app session: connects to the engine on launch, reads the profile and projects, keeps
 * the unconfirmed-change banner, the toast and a revision counter screens reload on. A reply
 * that leaves a change unconfirmed starts a few spaced automatic checks of that change.
 * Output: the session. */
export function useSession(): Session {
  const [engine, setEngine] = useState<EngineState>("connecting");
  const [unconfirmed, setUnconfirmed] = useState<Unconfirmed | null>(null);
  const tracking = useRef(false);
  const [profileCheck, setProfileCheck] = useState<{ busy: boolean } | null>(null);
  const profileTracking = useRef(false);
  const [stuckSignIn, setStuckSignIn] = useState<{ busy: boolean } | null>(null);
  const [profile, setProfile] = useState<Loaded<OwnerProfile | null>>({ kind: "loading" });
  const [projects, setProjects] = useState<Loaded<ProjectRow[]>>({ kind: "loading" });
  const [projectsMore, setProjectsMore] = useState(false);
  const [toast, setToast] = useState("");
  const [toastOpen, setToastOpen] = useState<Route | null>(null);
  const [revision, setRevision] = useState(0);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  function noteResult<T>(result: Result<T>): Result<T> {
    if (result.kind === "error" && result.code === "engine_unreachable") setEngine("down");
    if (result.kind === "error" && STUCK_SIGN_IN_CODES.includes(result.code)) void refreshStuckSignIn();
    return result;
  }

  async function refreshStuckSignIn() {
    const native = await api.status();
    setStuckSignIn(native.kind === "ok" && native.value.stuck_sign_in !== null ? { busy: false } : null);
  }

  function showToast(message: string, open?: Route) {
    if (toastTimer.current) clearTimeout(toastTimer.current);
    setToast(message);
    setToastOpen(open ?? null);
    toastTimer.current = setTimeout(() => {
      setToast("");
      setToastOpen(null);
    }, open ? 10_000 : 4000);
  }

  async function loadUnconfirmed(outcome?: ActionReceipt["outcome"] | null): Promise<PendingIntent | null> {
    const kept = noteResult(await api.keptChange());
    const intent = kept.kind === "ok" ? kept.value : null;
    setUnconfirmed((old) => intent && {
      intent,
      outcome: outcome !== undefined ? outcome : old?.intent.operation_id === intent.operation_id ? old.outcome : null,
      busy: false,
    });
    return intent;
  }

  async function settle(send: () => Promise<Result<IntentReceipt>>): Promise<boolean> {
    setUnconfirmed((old) => old && { ...old, busy: true });
    const result = noteResult(await send());
    const intent = await loadUnconfirmed(result.kind === "ok" ? result.value.outcome : undefined);
    const line = result.kind === "ok" ? api.settledLine(result.value) : null;
    if (line) showToast(line);
    if (!intent) {
      await Promise.all([refreshProfile(), refreshProjects()]);
      setRevision((value) => value + 1);
    }
    return intent === null || (result.kind === "ok" && result.value.outcome === "not_recorded_at_barrier");
  }

  async function track() {
    if (tracking.current) return;
    tracking.current = true;
    try {
      if (!(await loadUnconfirmed())) return;
      for (const pause of api.UNCONFIRMED_CHECKS_MS) {
        await new Promise((resolve) => setTimeout(resolve, pause));
        if (await settle(api.checkUnconfirmed)) return;
      }
    } finally {
      tracking.current = false;
    }
  }

  async function checkProfile(): Promise<boolean> {
    setProfileCheck((old) => old && { busy: true });
    const check = await api.checkAccountProfile();
    if (check === "pending" || check === "failed") {
      setProfileCheck({ busy: false });
      return false;
    }
    setProfileCheck(null);
    const line = api.profileSettledLine(check);
    if (line) showToast(line);
    return true;
  }

  async function trackProfile() {
    if (profileTracking.current) return;
    profileTracking.current = true;
    try {
      if (await checkProfile()) return;
      for (const pause of api.UNCONFIRMED_CHECKS_MS) {
        await new Promise((resolve) => setTimeout(resolve, pause));
        if (await checkProfile()) return;
      }
    } finally {
      profileTracking.current = false;
    }
  }

  async function refreshProfile() {
    setProfile(noteResult(await api.getProfile()));
  }

  async function refreshProjects() {
    const result = noteResult(await api.listProjects());
    setProjectsMore(result.kind === "ok" && result.value.more);
    setProjects(result.kind === "ok" ? { kind: "ok", value: result.value.items } : result);
  }

  async function start() {
    setEngine("connecting");
    setProfile({ kind: "loading" });
    setProjects({ kind: "loading" });
    const result = await api.connect();
    const state = engineState(result);
    setEngine(state);
    setStuckSignIn(result.kind === "ok" && result.value.stuck_sign_in !== null ? { busy: false } : null);
    if (state !== "running") return;
    await Promise.all([refreshProfile(), refreshProjects()]);
    setRevision((value) => value + 1);
  }

  useEffect(() => {
    const stop = watchUnconfirmed((settled) => { void (leavesProfileUnconfirmed(settled) ? trackProfile() : track()); });
    void start();
    return stop;
  }, []);

  return {
    engine, unconfirmed, profileCheck, stuckSignIn, profile, projects, projectsMore, toast, toastOpen, revision,
    retry: () => { void start(); },
    checkUnconfirmed: async () => { await settle(api.checkUnconfirmed); },
    checkProfile: async () => { await checkProfile(); },
    clearStuckSignIn: async () => {
      setStuckSignIn({ busy: true });
      showToast(api.stuckClearedLine(noteResult(await api.clearStuckSignIn())));
      await refreshStuckSignIn();
    },
    retryUnconfirmed: async () => { await settle(api.retryUnconfirmed); },
    dropUnconfirmed: async () => {
      setUnconfirmed((old) => old && { ...old, busy: true });
      const dropped = noteResult(await api.dropUnconfirmed());
      if (dropped.kind === "error") showToast(dropped.message);
      await loadUnconfirmed();
    },
    refreshProfile,
    refreshProjects,
    showToast,
    noteResult,
  };
}

export const SessionContext = createContext<Session | null>(null);

/** Reads the app session from context; screens call this instead of taking it as a prop.
 * Output: the session. Throws when used outside the app shell. */
export function useAppSession(): Session {
  const session = useContext(SessionContext);
  if (!session) throw new Error("useAppSession outside SessionContext");
  return session;
}

export const LOAD_CACHE_MAX = 64;
const loadCache = new Map<string, Result<unknown>>();

/** The cache key of one read: its name and the keys it reads with. Inputs: the read's cache
 * name and keys. Output: the key. */
export function loadKey(cache: string, keys: readonly unknown[]): string {
  return `${cache}\n${JSON.stringify(keys)}`;
}

/** Keeps a read's good reply for the next mount, in memory only, newest last, dropping the
 * oldest past LOAD_CACHE_MAX. Errors and not-ready replies leave the cached value as it was.
 * Inputs: the cache key and the reply. Output: none. */
export function cacheLoad<T>(key: string, result: Result<T>): void {
  if (result.kind !== "ok") return;
  loadCache.delete(key);
  loadCache.set(key, result);
  for (const oldest of loadCache.keys()) {
    if (loadCache.size <= LOAD_CACHE_MAX) break;
    loadCache.delete(oldest);
  }
}

/** The last good reply cached for a key. Input: the cache key, or null for an uncached read.
 * Output: the reply, or undefined. */
export function cachedLoad<T>(key: string | null): Result<T> | undefined {
  return key === null ? undefined : loadCache.get(key) as Result<T> | undefined;
}

const loadReads = new Map<string, Promise<Result<unknown>>>();

/** Starts a cached read, or joins the read of the same key already in flight, so screens that
 * mount together ask the engine once. A fresh read (a reload) never joins: it starts its own and
 * later joiners share that newer one. Inputs: the shared key (null reads alone), whether to read
 * fresh and the read. Output: the reply. */
export function sharedLoad<T>(key: string | null, fresh: boolean, read: () => Promise<Result<T>>): Promise<Result<T>> {
  if (key === null) return read();
  const running = loadReads.get(key);
  if (running && !fresh) return running as Promise<Result<T>>;
  const started = read();
  loadReads.set(key, started);
  const clear = () => { if (loadReads.get(key) === started) loadReads.delete(key); };
  started.then(clear, clear);
  return started;
}

/** A choice the owner saved on a control and the read value it replaced. */
export interface Held<T> {
  choice: T;
  from: T;
}

/** What a control keeps after a save: the choice once the engine applied it, so the control
 * never flips back to the old value while the reload is on its way, and nothing after a refused
 * or unconfirmed save, so the read value shows again. Inputs: the held choice and the save's
 * note, empty when the engine applied it. Output: the choice to keep, or null. */
export function afterSave<T>(held: Held<T>, note: string): Held<T> | null {
  return note === "" ? held : null;
}

/** The value a control shows: the held choice while the read still holds the value it replaced,
 * otherwise the read value, once a reload or another window moved it. Inputs: the held choice or
 * null, the read value and how two values compare. Output: the value. */
export function shownChoice<T>(held: Held<T> | null, current: T, same: (a: T, b: T) => boolean = Object.is): T {
  return held !== null && same(held.from, current) ? held.choice : current;
}

/** Starts holding a picked value and gives the call that settles it with the save's note. */
export type Hold<T> = (choice: T) => (note: string) => void;

/** Keeps a control on the owner's pick from the moment of the pick until a read moves the value
 * it replaced, or until its save is refused: without this a control falls back to the old read
 * value after its save, and a native select cannot be picked back to the value it already shows.
 * Inputs: the value read and how two values compare. Output: the value to show and `hold`. */
export function useHeld<T>(current: T, same: (a: T, b: T) => boolean = Object.is): [T, Hold<T>] {
  const [held, setHeld] = useState<Held<T> | null>(null);
  const moved = held !== null && !same(held.from, current);
  useEffect(() => { if (moved) setHeld(null); }, [moved]);
  const hold: Hold<T> = (choice) => {
    const kept = { choice, from: current };
    setHeld(kept);
    return (note) => setHeld(afterSave(kept, note));
  };
  return [shownChoice(held, current, same), hold];
}

/** Loads one engine read for a screen and reloads it when the session revision or the given
 * keys change. A read given a cache name first shows its last good reply from an earlier mount,
 * then refreshes in the background; a failed refresh shows its error as an uncached read does.
 * Cached reads mounting together share one engine read; a reload always reads again.
 * Inputs: the read, its keys and an optional cache name. Output: the loaded value and a reload action. */
export function useLoad<T>(read: () => Promise<Result<T>>, keys: unknown[] = [], cache?: string): [Loaded<T>, () => void] {
  const session = useContext(SessionContext);
  const key = cache === undefined ? null : loadKey(cache, keys);
  const [value, setValue] = useState<Loaded<T>>(() => cachedLoad<T>(key) ?? { kind: "loading" });
  const [tick, setTick] = useState(0);
  const shownKey = useRef(key);
  const fresh = useRef(false);
  useEffect(() => {
    let live = true;
    const cached = cachedLoad<T>(key);
    const same = shownKey.current === key;
    shownKey.current = key;
    setValue((old) => cached ?? (old.kind === "ok" && same ? old : { kind: "loading" }));
    const reading = sharedLoad(key === null ? null : `${key}\n${session?.revision ?? 0}`, fresh.current, read);
    fresh.current = false;
    void reading.then((result) => {
      if (!live) return;
      const noted = session ? session.noteResult(result) : result;
      if (key !== null) cacheLoad(key, noted);
      setValue(noted);
    });
    return () => { live = false; };
  }, [session?.revision, tick, key, ...keys]);
  return [value, () => { fresh.current = true; setTick((old) => old + 1); }];
}
