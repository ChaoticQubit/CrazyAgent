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
exports.sidebarContext = sidebarContext;
const vscode = __importStar(require("vscode"));
const model_js_1 = require("./model.js");
const tree_js_1 = require("./tree.js");
const PROJECT_FOLDER = "Project";
function sidebarContext(context, engine) {
    const tree = new tree_js_1.RowTree("crazyagent.context", (row, item) => {
        if (row.path === undefined)
            return;
        const file = vscode.Uri.file(row.path);
        item.resourceUri = file;
        item.iconPath = vscode.ThemeIcon.File;
        item.command = { command: "vscode.open", title: "Open", arguments: [file] };
    });
    const load = async () => {
        const folders = vscode.workspace.workspaceFolders ?? [];
        const project = (folders.find((folder) => folder.name === PROJECT_FOLDER) ?? folders[0])?.uri;
        if (project === undefined || project.scheme !== "file") {
            tree.say("Open the CrazyAgent workspace to see the project's brief and documents.");
            tree.show([]);
            return;
        }
        const folder = (0, model_js_1.contextFolder)(project.fsPath, await found(vscode.Uri.joinPath(project, ".crazyagent")));
        const [documents, research] = await Promise.all([names(`${folder}/documents`), names(`${folder}/research`)]);
        tree.say(undefined);
        tree.show((0, model_js_1.contextLinks)(folder, documents, research));
    };
    const refresh = (0, tree_js_1.coalesced)(load);
    context.subscriptions.push(tree, engine.on("workspace_changed", refresh), vscode.workspace.onDidChangeWorkspaceFolders(refresh), tree.view.onDidChangeVisibility((change) => {
        if (change.visible)
            refresh();
    }), vscode.commands.registerCommand("crazyagent.context.refresh", refresh));
    refresh();
}
async function found(path) {
    try {
        const { type } = await vscode.workspace.fs.stat(path);
        return { directory: (type & vscode.FileType.Directory) !== 0, symlink: (type & vscode.FileType.SymbolicLink) !== 0 };
    }
    catch {
        return null;
    }
}
async function names(folder) {
    try {
        const entries = await vscode.workspace.fs.readDirectory(vscode.Uri.file(folder));
        return entries.filter(([, type]) => type === vscode.FileType.File).map(([name]) => name);
    }
    catch {
        return null;
    }
}
