"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.MEMORY_PAGES_MAX = exports.ROWS_PER_GROUP_MAX = exports.LABEL_CHARS_MAX = void 0;
exports.memoryGroups = memoryGroups;
exports.agentGroups = agentGroups;
exports.changeGroups = changeGroups;
exports.contextLinks = contextLinks;
exports.contextFolder = contextFolder;
exports.nextPage = nextPage;
exports.plainLine = plainLine;
exports.LABEL_CHARS_MAX = 80;
exports.ROWS_PER_GROUP_MAX = 200;
exports.MEMORY_PAGES_MAX = 50;
const UNSAFE = /[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/g;
const UNSAFE_CHAR = /[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/;
const OPEN_CHANGES = new Set(["pending", "conflict", "updating"]);
const MEMORY_GROUPS = [
    { label: "Waiting for you", kind: "memory.pending", states: ["pending"] },
    { label: "Approved", kind: "memory.approved", states: ["approved"] },
    { label: "Set aside", kind: "memory.aside", states: ["rejected", "removed", "retracted"] },
];
function memoryGroups(memories) {
    return MEMORY_GROUPS.map(({ label, kind, states }) => group(`memories:${kind}`, label, memories
        .filter((memory) => states.includes(memory.state))
        .map((memory) => ({
        key: `memory:${memory.id}`,
        label: plainLine(memory.text, exports.LABEL_CHARS_MAX),
        description: `revision ${memory.revision}`,
        kind,
        children: [],
        memory,
    })))).filter(nonEmpty);
}
function agentGroups(agents) {
    const running = (agent) => agent.status !== "stopped" && agent.status !== "failed";
    const groups = [
        ["Master", agents.filter((agent) => agent.role === "master")],
        ["Running", agents.filter((agent) => agent.role === "sub" && running(agent))],
        ["Stopped", agents.filter((agent) => agent.role === "sub" && !running(agent))],
    ];
    return groups.map(([label, members]) => group(`agents:${label}`, label, members.map((agent) => ({
        key: `agent:${agent.id}`,
        label: plainLine(agent.label, exports.LABEL_CHARS_MAX),
        description: plainLine(agent.status_label, exports.LABEL_CHARS_MAX),
        kind: ["agent", agent.message ? "agent.message" : "", agent.worktree ? "agent.worktree" : ""].filter(Boolean).join(" "),
        children: [],
        agent,
    })))).filter(nonEmpty);
}
function changeGroups(changes, now) {
    const restorable = (change) => change.state === "discarded" && change.restorable_until !== null && change.restorable_until > now;
    const groups = [
        ["Open", changes.filter((change) => OPEN_CHANGES.has(change.state))],
        ["Can restore", changes.filter(restorable)],
    ];
    return groups.map(([label, members]) => group(`changes:${label}`, label, members.map((change) => ({
        key: `change:${change.id}`,
        label: `${plainLine(change.agent, exports.LABEL_CHARS_MAX)}: ${plainLine(change.title, exports.LABEL_CHARS_MAX) || "Changes"}`,
        description: `${change.files.length} ${change.files.length === 1 ? "file" : "files"}`,
        kind: "change",
        children: [],
        change,
    })))).filter(nonEmpty);
}
function contextLinks(folder, documents, research) {
    const base = folder.replace(/\/+$/, "");
    const files = (directory, names) => (names ?? [])
        .filter((name) => name !== "" && !name.startsWith(".") && !name.includes("/") && !UNSAFE_CHAR.test(name))
        .sort((a, b) => a.localeCompare(b))
        .map((name) => ({
        key: `context:${directory}/${name}`,
        label: name,
        description: "",
        kind: "context.file",
        children: [],
        path: `${base}/${directory}/${name}`,
    }));
    const brief = { key: "context:brief.md", label: "Brief", description: "brief.md", kind: "context.file", children: [], path: `${base}/brief.md` };
    const groups = [group("context:documents", "Documents", files("documents", documents)), group("context:research", "Research", files("research", research))];
    return [brief, ...groups.filter(nonEmpty)];
}
function contextFolder(project, found) {
    const base = project.replace(/\/+$/, "");
    return found !== null && found.directory && !found.symlink ? `${base}/.crazyagent/context` : `${base}/context`;
}
function nextPage(page, pages) {
    if (page.length === 0 || pages >= exports.MEMORY_PAGES_MAX)
        return null;
    return page[page.length - 1].id;
}
function plainLine(text, max) {
    const line = text.replace(UNSAFE, " ").replace(/\s+/g, " ").trim();
    const chars = [...line];
    return chars.length <= max ? line : `${chars.slice(0, max - 3).join("")}...`;
}
function group(key, label, children) {
    const shown = children.slice(0, exports.ROWS_PER_GROUP_MAX);
    const description = shown.length < children.length ? `${shown.length} of ${children.length}` : String(children.length);
    return { key, label, description, kind: "group", children: shown };
}
function nonEmpty(row) {
    return row.children.length > 0;
}
