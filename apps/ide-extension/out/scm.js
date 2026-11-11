"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.registerChanges = registerChanges;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("./scm-model.js");
const POLL_MS = 5_000;
const PROBLEM_GROUP = "crazyagent.problem";
const UNREACHABLE = "CrazyAgent couldn't reach the engine. The changes come back here when it is running.";
const DISCARD_ASK = "Discard these changes? You can restore them for 7 days.";
const DISCARD = "Discard";
const DISCARDED = "Discarded. You can restore these changes for 7 days.";
const RESTORED = "Restored. The changes are ready to review again.";
const ICONS = { A: "diff-added", M: "diff-modified", D: "diff-removed", R: "diff-renamed" };
function registerChanges(context, engine) {
    const control = vscode.scm.createSourceControl("crazyagent", "CrazyAgent changes");
    control.inputBox.visible = false;
    const groups = new Map();
    const shownGroups = new Map();
    const sets = new Map();
    let loading = false;
    let again = false;
    const show = (rows) => {
        const current = [...groups.keys()];
        const same = rows.length === current.length && rows.every((row, at) => row.id === current[at]);
        if (!same) {
            for (const group of groups.values())
                group.dispose();
            groups.clear();
            for (const row of rows) {
                const group = control.createResourceGroup(row.id, row.label);
                group.hideWhenEmpty = false;
                groups.set(row.id, group);
            }
        }
        for (const row of rows) {
            const group = groups.get(row.id);
            if (group === undefined)
                throw new Error("every row has its Source Control group");
            group.label = row.label;
            group.resourceStates = row.states;
        }
        control.count = [...shownGroups.values()].reduce((count, shown) => count + (shown.accept ? shown.resources.length : 0), 0);
    };
    const problem = (label) => {
        sets.clear();
        shownGroups.clear();
        show([{ id: PROBLEM_GROUP, label, states: [] }]);
    };
    const load = async () => {
        let reply;
        try {
            reply = await engine.request("ide.change_sets", {});
        }
        catch {
            problem(UNREACHABLE);
            return;
        }
        if (reply.kind !== "ok")
            return problem((0, scm_model_js_1.refusal)(reply.code));
        if (!Array.isArray(reply.data))
            return problem((0, scm_model_js_1.refusal)("changes_outcome_unknown"));
        const listed = (0, scm_model_js_1.scmGroups)(reply.data, Date.now());
        sets.clear();
        shownGroups.clear();
        for (const set of reply.data)
            sets.set(set.id, set);
        const rows = listed.map((shown) => {
            const set = sets.get(shown.set);
            if (set === undefined)
                throw new Error("every group names a listed change set");
            shownGroups.set(shown.id, shown);
            return { id: shown.id, label: shown.label, states: shown.resources.map((resource) => resourceState(set, resource)) };
        });
        show(rows);
    };
    const refresh = () => {
        if (loading) {
            again = true;
            return;
        }
        loading = true;
        void load().finally(() => {
            loading = false;
            if (again) {
                again = false;
                refresh();
            }
        });
    };
    const target = (group, allowed) => {
        const shown = group === undefined ? undefined : shownGroups.get(group.id);
        const set = shown === undefined ? undefined : sets.get(shown.set);
        if (shown === undefined || set === undefined || !allowed(shown)) {
            void vscode.window.showErrorMessage((0, scm_model_js_1.refusal)("changes_not_found"));
            return null;
        }
        return set;
    };
    const ask = async (name, args) => {
        try {
            const reply = await engine.request(name, args);
            if (reply.kind === "ok")
                return { value: reply.data };
            void vscode.window.showErrorMessage((0, scm_model_js_1.refusal)(reply.code));
            return null;
        }
        catch {
            void vscode.window.showErrorMessage((0, scm_model_js_1.refusal)("changes_outcome_unknown"));
            return null;
        }
        finally {
            refresh();
        }
    };
    const accept = async (group) => {
        const set = target(group, (shown) => shown.accept);
        if (set === null)
            return;
        const message = await vscode.window.showInputBox({
            title: `Accept ${(0, scm_model_js_1.plainLine)(set.agent)}: ${(0, scm_model_js_1.plainLine)(set.title)}`,
            prompt: "Commit message. CrazyAgent lands these changes on your branch as one commit.",
            value: set.message,
            ignoreFocusOut: true,
            validateInput: (text) => (text.trim() === "" ? "Write a commit message." : null),
        });
        if (message === undefined)
            return;
        const done = await ask("ide.accept", { id: set.id, head: set.head, message });
        if (done === null)
            return;
        const note = (0, scm_model_js_1.acceptedNote)(done.value);
        if (done.value.kind === "landed")
            void vscode.window.showInformationMessage(note);
        else
            void vscode.window.showWarningMessage(note);
    };
    const discard = async (group) => {
        const set = target(group, (shown) => shown.discard);
        if (set === null)
            return;
        const detail = `${(0, scm_model_js_1.plainLine)(set.agent)}: ${(0, scm_model_js_1.plainLine)(set.title)}`;
        const choice = await vscode.window.showWarningMessage(DISCARD_ASK, { modal: true, detail }, DISCARD);
        if (choice !== DISCARD)
            return;
        const done = await ask("ide.discard", { id: set.id, head: set.head });
        if (done !== null)
            void vscode.window.showInformationMessage(DISCARDED);
    };
    const restore = async (group) => {
        const set = target(group, (shown) => shown.restore);
        if (set === null)
            return;
        const done = await ask("ide.restore", { id: set.id });
        if (done !== null)
            void vscode.window.showInformationMessage(RESTORED);
    };
    const timer = setInterval(refresh, POLL_MS);
    context.subscriptions.push(control, { dispose: () => clearInterval(timer) }, engine.on("change_sets_changed", refresh), vscode.commands.registerCommand("crazyagent.changes.accept", accept), vscode.commands.registerCommand("crazyagent.changes.discard", discard), vscode.commands.registerCommand("crazyagent.changes.restore", restore), vscode.commands.registerCommand("crazyagent.changes.refresh", refresh));
    refresh();
}
function resourceState(set, resource) {
    const file = vscode.Uri.file(resource.file);
    const base = resource.base === null ? null : vscode.Uri.from({ scheme: scm_model_js_1.BASE_SCHEME, path: resource.base, query: set.head });
    const head = resource.head === null ? null : vscode.Uri.from({ scheme: scm_model_js_1.HEAD_SCHEME, path: resource.head, query: set.head });
    const right = resource.config === null ? head : vscode.Uri.from({ scheme: scm_model_js_1.CONFIG_SCHEME, path: resource.config, query: set.head });
    const name = (0, scm_model_js_1.plainLine)(resource.path);
    let command;
    if (resource.open === "diff" && base !== null && right !== null) {
        command = { command: "vscode.diff", title: "Open changes", arguments: [base, right, `${name} (${(0, scm_model_js_1.plainLine)(set.agent)})`] };
    }
    else if (resource.open === "base" && base !== null) {
        command = { command: "vscode.open", title: "Open the file before the agent's changes", arguments: [base] };
    }
    else if (resource.open === "file") {
        command = { command: "vscode.open", title: "Open the file", arguments: [file] };
    }
    return {
        resourceUri: file,
        command,
        decorations: {
            strikeThrough: resource.strikeThrough,
            tooltip: resource.tooltip,
            iconPath: new vscode.ThemeIcon(ICONS[resource.letter] ?? "diff-modified"),
        },
    };
}
