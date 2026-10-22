import type { AccountPool, ModelAccessCollection, ModelAccessRecord, ModelAccessEdit, ModelAuthentication, ModelConnection, ProjectDetail } from "./engine-client.ts";

export interface ModelAccessForm {
  collection: ModelAccessCollection;
  action: "create" | "replace" | "remove";
  ticket: string;
  id: string;
  provider: string;
  harness: string;
  protocol: string;
  account: string;
  endpoint: "http" | "native";
  url: string;
  model: string;
  author: string;
  auth: ModelAuthentication["method"];
  credential: string;
  members: string;
  projects: string;
  roles: string;
  fallback: boolean;
  providerQuotaPolicy: NonNullable<AccountPool["provider_quota_policy"]>;
}

export interface ManagedAccountInput {
  provider: "codex" | "claude";
  label: string;
}

export function managedAccountInput(provider: unknown, label: unknown): ManagedAccountInput {
  if (provider !== "codex" && provider !== "claude") throw new Error("Choose Codex or Claude.");
  if (typeof label !== "string") throw new Error("Enter an optional account label.");
  if (/[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(label)) {
    throw new Error("The account label must fit on one line without display controls.");
  }
  const value = label.trim();
  if (new TextEncoder().encode(value).length > 96) throw new Error("Account labels allow at most 96 UTF-8 bytes.");
  return { provider, label: value };
}

export function configuredAccountInput(provider: unknown, label: unknown, endpoint: unknown, model: unknown): {
  provider: "api_key" | "local"; label: string; url: string; model: string;
} {
  if (provider !== "api_key" && provider !== "local") throw new Error("Choose API key or local endpoint setup.");
  const value = managedAccountInput("codex", label).label;
  if (typeof endpoint !== "string" || new TextEncoder().encode(endpoint).length > 2048) throw new Error("Endpoint allows at most 2048 UTF-8 bytes.");
  if (!endpoint || /[\s\\\u0000-\u001f\u007f-\u009f]/u.test(endpoint)) throw new Error("Enter a valid HTTP or HTTPS endpoint without credentials, query or fragment.");
  let url: URL;
  try { url = new URL(endpoint); }
  catch { throw new Error(provider === "local" ? "No-key setup requires an HTTP loopback endpoint." : "Enter a valid HTTP or HTTPS endpoint."); }
  const host = url.hostname.toLowerCase();
  const loopback = host === "localhost" || host === "[::1]" || /^127(?:\.\d{1,3}){3}$/u.test(host);
  if (provider === "local" && (url.protocol !== "http:" || !loopback)) throw new Error("No-key setup requires an HTTP loopback endpoint.");
  if (!["http:", "https:"].includes(url.protocol) || !host || url.username || url.password || url.search || url.hash || endpoint.includes("?") || endpoint.includes("#")) {
    throw new Error("Enter an HTTP or HTTPS endpoint without credentials, query or fragment.");
  }
  if (typeof model !== "string" || /[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(model) || !model.trim() || new TextEncoder().encode(model.trim()).length > 256) {
    throw new Error("A model name is required on one line, at most 256 UTF-8 bytes.");
  }
  return { provider, label: value, url: endpoint, model: model.trim() };
}

export function newForm(collection: ModelAccessCollection, ticket: string): ModelAccessForm {
  return { collection, ticket, action: "create", id: "", provider: "", harness: "", protocol: "", account: "", endpoint: "http", url: "", model: "", author: "", auth: "none", credential: "", members: "", projects: "", roles: "", fallback: false, providerQuotaPolicy: "bounded_attempt" };
}

function quotaPolicy(value: unknown): ModelAccessForm["providerQuotaPolicy"] {
  if (value !== "bounded_attempt" && value !== "strict") throw new Error("Provider quota policy must be bounded_attempt or strict.");
  return value;
}

export function recordId(record: ModelAccessRecord): string {
  switch (record.kind) {
    case "account": return record.account_ref;
    case "connection": return record.connection.id;
    case "pool": return record.pool.id;
  }
}

export function formFromRecord(record: ModelAccessRecord, ticket: string, remove = false): ModelAccessForm {
  const collection = record.kind === "account" ? "accounts" : record.kind === "connection" ? "connections" : "pools";
  const form = { ...newForm(collection, ticket), id: recordId(record), action: remove ? "remove" as const : "replace" as const };
  if (record.kind === "connection") {
    const c = record.connection;
    return { ...form, provider: c.provider, harness: c.harness, protocol: c.protocol, account: c.account_ref, endpoint: c.endpoint.kind, url: c.endpoint.kind === "http" ? c.endpoint.url : "", model: c.model.id, author: c.model.author, auth: c.auth.method, credential: "credential_ref" in c.auth ? c.auth.credential_ref : "profile_ref" in c.auth ? c.auth.profile_ref : "" };
  }
  if (record.kind === "pool") {
    const p = record.pool;
    return { ...form, members: p.members.join(", "), projects: p.projects.join(", "), roles: p.roles.join(", "), fallback: p.fallback_allowed, providerQuotaPolicy: quotaPolicy(p.provider_quota_policy === undefined ? "bounded_attempt" : p.provider_quota_policy) };
  }
  return form;
}

function reference(value: string): string {
  if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(value) || value.includes("..")) throw new Error("References require 1 to 128 safe ASCII letters, digits, _, -, . or :; no '..'. Enter a stored reference, never a secret.");
  return value;
}

function references(value: string, allowEmpty = false): string[] {
  if (allowEmpty && value.trim() === "") return [];
  const items = value.split(",").map((item) => reference(item.trim()));
  if (items.length > 64 || new Set(items).size !== items.length) throw new Error("Enter 1 to 64 unique references separated by commas.");
  return items;
}

export function setPoolConnection(form: ModelAccessForm, id: string, selected: boolean): ModelAccessForm {
  if (form.collection !== "pools" || form.action === "remove") throw new Error("Choose connections only for an editable pool.");
  const connection = reference(id);
  const members = references(form.members, true);
  if (members.includes(connection) === selected) return form;
  if (selected && members.length === 64) throw new Error("An account pool allows at most 64 connections.");
  return { ...form, members: (selected ? [...members, connection] : members.filter((member) => member !== connection)).join(", ") };
}

export function withPoolProject(form: ModelAccessForm, project: Pick<ProjectDetail, "project_id" | "fresh">): ModelAccessForm {
  if (form.collection !== "pools" || form.action !== "replace" || !project.fresh) {
    throw new Error("Inspect a saved pool and open the selected project before confirming its assignment.");
  }
  const id = reference(project.project_id);
  const projects = references(form.projects, true);
  const roles = references(form.roles, true);
  if (!projects.includes(id)) projects.push(id);
  if (!roles.includes("project_leader")) roles.push("project_leader");
  if (projects.length > 64 || roles.length > 64) throw new Error("A pool allows at most 64 project and 64 role assignments.");
  return { ...form, projects: projects.join(", "), roles: roles.join(", ") };
}

export function editFromForm(form: ModelAccessForm): ModelAccessEdit {
  const id = reference(form.id);
  if (form.collection === "accounts") {
    if (form.action === "replace") throw new Error("Account references support add and remove only.");
    return { kind: form.action === "remove" ? "remove_account" : "add_account", account_ref: id };
  }
  if (form.action === "remove") return { kind: form.collection === "connections" ? "remove_connection" : "remove_pool", id };
  if (form.collection === "pools") {
    const policy = quotaPolicy(form.providerQuotaPolicy);
    return { kind: form.action === "create" ? "create_pool" : "replace_pool", pool: { id, members: references(form.members), projects: references(form.projects, true), roles: references(form.roles, true), fallback_allowed: form.fallback, ...(policy === "strict" ? { provider_quota_policy: policy } : {}) } };
  }
  if (!/^[A-Za-z0-9_.:/@+-]{1,256}$/.test(form.model)) throw new Error("Model ID requires 1 to 256 safe ASCII model identifier characters.");
  if (form.endpoint === "native" && form.protocol !== "native") throw new Error("Native endpoints require the native protocol.");
  if (form.endpoint === "http") {
    let url: URL;
    try { url = new URL(form.url); } catch { throw new Error("Enter an absolute HTTP or HTTPS endpoint without credentials, query or fragment."); }
    if (form.protocol === "native" || form.auth === "native_managed" || new TextEncoder().encode(form.url).length > 2048 || /[\s\u0000-\u001f\u007f\\?#]/.test(form.url) || !form.url.includes("://") || !["http:", "https:"].includes(url.protocol) || !url.host || url.username || url.password || url.search || url.hash) {
      throw new Error("HTTP endpoints require a non-native protocol and no credentials, query, fragment or whitespace. Native-managed authentication requires a native endpoint.");
    }
  }
  const auth: ModelAuthentication = form.auth === "none" ? { method: "none" } : form.auth === "native_managed" ? { method: "native_managed", profile_ref: reference(form.credential) } : { method: form.auth, credential_ref: reference(form.credential) };
  const connection: ModelConnection = { id, provider: reference(form.provider), harness: reference(form.harness), protocol: reference(form.protocol), account_ref: reference(form.account), endpoint: form.endpoint === "native" ? { kind: "native" } : { kind: "http", url: form.url }, model: { id: form.model, author: reference(form.author) }, auth };
  return { kind: form.action === "create" ? "create_connection" : "replace_connection", connection };
}

export function rebaseForm(form: ModelAccessForm, basis: { ticket: string; collection: ModelAccessCollection; record?: ModelAccessRecord }): ModelAccessForm {
  if (form.collection !== basis.collection || (form.action !== "create" && (!basis.record || recordId(basis.record) !== form.id))) throw new Error("Refresh and inspect the same metadata identity before reviewing this retained form.");
  return { ...form, ticket: basis.ticket };
}
