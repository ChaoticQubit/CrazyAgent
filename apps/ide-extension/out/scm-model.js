"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.COMMENT_SENT = exports.REFUSAL_CODES = exports.BASE_SCHEME = void 0;
exports.plainLine = plainLine;
exports.refusal = refusal;
exports.acceptedNote = acceptedNote;
exports.basePath = basePath;
exports.parseBasePath = parseBasePath;
exports.scmGroups = scmGroups;
exports.commentTarget = commentTarget;
exports.commentRefusal = commentRefusal;
exports.BASE_SCHEME = "crazyagent-base";
const SET_ID = /^[0-9a-f]{32}$/;
const CONFLICT_SHOWN = 5;
const UNSAFE = /[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;
const LETTERS = { added: "A", modified: "M", deleted: "D", renamed: "R" };
const STATUS_WORDS = { added: "Added", modified: "Modified", deleted: "Deleted", renamed: "Renamed" };
const KIND_MARKS = {
    binary: "Binary file, no text diff",
    symlink: "Symbolic link: points to another path",
    gitlink: "Submodule commit",
};
const MARKS = {
    symlink: KIND_MARKS.symlink,
    executable: "Can run as a program",
    mode: "File mode changed",
    attributes: ".gitattributes: changes how Git treats files",
    modules: ".gitmodules: changes the project's submodules",
};
const SUFFIXES = {
    open: "",
    unread: "",
    waiting: " (updated, waiting for the agent)",
    resolving: " (the agent is updating it)",
    restorable: " (discarded, can be restored)",
};
const CONFLICT_SUFFIX = " (conflicts with your branch)";
const UNREAD = " (CrazyAgent couldn't read these changes)";
const TOO_LARGE = " (too large to show here)";
const REFUSALS = {
    changes_owner_dirty: "Your project folder has uncommitted changes to tracked files. Commit or stash them, then accept again.",
    changes_detached_head: "Your project folder isn't on a branch. Check out the branch these changes should land on, then accept again.",
    changes_git_busy: "Git is in the middle of a merge, rebase, cherry-pick or bisect in your project folder. Finish or stop it, then accept again.",
    changes_conflict: "These changes conflict with your branch. Ask the agent to update, or discard.",
    changes_stale: "These changes moved on since you opened them. Review them again, then accept.",
    changes_branch_moved: "Your branch moved while these changes were landing, so nothing changed. Review and accept again.",
    changes_git_unsupported: "The Git on this computer can't land changes. Update Git, then try again.",
    changes_ignored_files: "These changes add files your project's ignore rules leave out. Ask the agent to remove them.",
    changes_message_invalid: "The commit message is empty, too long or has characters Git can't keep. Edit it, then accept again.",
    changes_restore_expired: "These changes can no longer be restored. Discarded changes are kept for 7 days.",
    changes_agent_has_pending: "This agent has newer changes waiting. Accept or discard those first.",
    changes_agent_busy: "The agent is working right now. Try again when it finishes.",
    changes_unavailable: "Landing changes isn't ready in this engine yet.",
    changes_git_failed: "Git couldn't finish this step in your project folder. Try again; if it keeps failing, check the folder with Git.",
    changes_not_found: "These changes are no longer there. The list shows what is left.",
    changes_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
    changes_owner_identity_missing: "Git has no name and email for you in your project folder. Set user.name and user.email, then accept again.",
    changes_owner_unavailable: "Your project folder couldn't be read. Check that it is still there, then try again.",
    changes_too_large: "These changes are larger than CrazyAgent can show or land. Ask the agent to split them up.",
    changes_workspace_invalid: "The agent's working copy was moved or changed outside CrazyAgent, so these changes can't land from it.",
};
exports.REFUSAL_CODES = Object.keys(REFUSALS);
function plainLine(text) {
    return text.replace(UNSAFE, "\ufffd");
}
function refusal(code) {
    return Object.hasOwn(REFUSALS, code) ? REFUSALS[code] : `Something went wrong. Try again. (${plainLine(code)})`;
}
function acceptedNote(outcome) {
    if (outcome.kind === "landed")
        return `Accepted as ${outcome.commit.slice(0, 7)} on ${plainLine(outcome.branch)}.`;
    const shown = outcome.files.slice(0, CONFLICT_SHOWN).map(plainLine).join(", ");
    const more = outcome.files.length > CONFLICT_SHOWN ? ` and ${outcome.files.length - CONFLICT_SHOWN} more` : "";
    return `These files also changed on your branch: ${shown}${more}. Ask the agent to update, or discard.`;
}
function basePath(id, path) {
    return `/${id}/${path}`;
}
function parseBasePath(text) {
    const match = /^\/([^/]+)\/(.+)$/s.exec(text);
    if (match === null || !SET_ID.test(match[1]))
        return null;
    const parts = match[2].split("/");
    if (parts.some((part) => part === "" || part === "." || part === ".."))
        return null;
    return { id: match[1], path: match[2] };
}
function context(set, now) {
    switch (set.state) {
        case "pending":
        case "conflict":
            return "open";
        case "updating":
            return set.waiting ? "waiting" : "resolving";
        case "discarded":
            return set.restorable_until !== null && set.restorable_until > now ? "restorable" : null;
        case "accepted":
            return null;
    }
}
function opens(file) {
    if (file.kind !== "text")
        return file.status === "deleted" ? "none" : "file";
    return file.status === "deleted" ? "base" : "diff";
}
function tooltip(file) {
    const status = file.status === "renamed" && file.old_path ? `Renamed from ${plainLine(file.old_path)}` : STATUS_WORDS[file.status];
    const marks = [KIND_MARKS[file.kind], ...file.marks.map((mark) => MARKS[mark] ?? `Marked ${plainLine(mark).replaceAll("_", " ")}`)];
    return [...new Set([status, ...marks.filter((mark) => mark !== undefined)])].join(", ");
}
function resource(set, file) {
    const open = opens(file);
    return {
        path: file.path,
        file: `${set.worktree.replace(/\/+$/, "")}/${file.path}`,
        base: open === "diff" || open === "base" ? basePath(set.id, file.path) : null,
        open,
        letter: LETTERS[file.status],
        tooltip: tooltip(file),
        strikeThrough: file.status === "deleted",
    };
}
function scmGroups(sets, now) {
    const groups = [];
    for (const set of sets) {
        const state = context(set, now);
        if (state === null)
            continue;
        const shown = state === "open" && set.problem !== null ? "unread" : state;
        const name = `${plainLine(set.agent)}: ${plainLine(set.title) || "Changes"}`;
        const suffix = set.state === "conflict" ? CONFLICT_SUFFIX : SUFFIXES[shown];
        const problem = set.problem === null ? "" : set.problem === "changes_too_large" ? TOO_LARGE : UNREAD;
        groups.push({
            id: `crazyagent.${set.id}.${shown}`,
            set: set.id,
            label: `${name}${suffix}${problem}`,
            context: shown,
            accept: shown === "open",
            discard: shown === "open" || shown === "unread" || shown === "waiting",
            restore: shown === "restorable",
            resources: set.problem === null ? set.files.map((file) => resource(set, file)) : [],
        });
    }
    return groups;
}
const COMMENT_STATES = new Set(["pending", "conflict", "updating"]);
const COMMENT_REFUSALS = {
    changes_closed: "These changes were already accepted or discarded, so the agent can't act on this comment.",
    team_conversation_full: "Too many of your messages are waiting for the master. Send more once it has answered.",
    team_message_invalid: "The message is empty or too long.",
    project_setup_missing: "This project was created before project setup existed.",
};
exports.COMMENT_SENT = "Sent to the team conversation with its file and line.";
function commentTarget(sets, fsPath) {
    for (const set of sets) {
        if (!COMMENT_STATES.has(set.state) || set.problem !== null)
            continue;
        const root = `${set.worktree.replace(/\/+$/, "")}/`;
        if (!fsPath.startsWith(root))
            continue;
        const path = fsPath.slice(root.length);
        if (set.files.some((file) => file.path === path && file.status !== "deleted"))
            return { id: set.id, path };
    }
    return null;
}
function commentRefusal(code) {
    return Object.hasOwn(COMMENT_REFUSALS, code) ? COMMENT_REFUSALS[code] : refusal(code);
}
