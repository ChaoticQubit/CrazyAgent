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
exports.readonly = exports.EDIT_ANYWAY_COMMAND = void 0;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("./scm-model.js");
const worktree_js_1 = require("./worktree.js");
exports.EDIT_ANYWAY_COMMAND = "crazyagent.editAnyway";
const readonly = (context, engine) => {
    context.subscriptions.push(vscode.commands.registerCommand(exports.EDIT_ANYWAY_COMMAND, () => editAnyway(engine)));
};
exports.readonly = readonly;
async function editAnyway(engine) {
    const uri = vscode.window.activeTextEditor?.document.uri;
    const folder = uri === undefined ? undefined : vscode.workspace.getWorkspaceFolder(uri);
    const agent = folder === undefined ? null : (0, worktree_js_1.agentOf)(folder.uri.fsPath);
    const file = uri === undefined || folder === undefined ? null : (0, worktree_js_1.insideFolder)(folder.uri.fsPath, uri.fsPath);
    if (folder === undefined || agent === null || file === null) {
        await vscode.window.showInformationMessage("This file is not in a running agent's worktree.");
        return;
    }
    const choice = await vscode.window.showWarningMessage((0, worktree_js_1.editAnywayQuestion)(folder.name), {
        modal: true,
        detail: "Your edit becomes part of the agent's change set, marked as edited by you, and the agent is told.",
    }, worktree_js_1.EDIT_ANYWAY);
    if (choice !== worktree_js_1.EDIT_ANYWAY)
        return;
    const reply = await engine.request("ide.owner_edit", { agent, path: file });
    if (reply.kind !== "ok") {
        await vscode.window.showErrorMessage((0, scm_model_js_1.unconfirmed)(reply.code) ?? `The file stays read-only (${reply.code}).`);
        return;
    }
    await vscode.commands.executeCommand("workbench.action.files.setActiveEditorWriteableInSession");
}
