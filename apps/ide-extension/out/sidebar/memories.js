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
exports.sidebarMemories = sidebarMemories;
const vscode = __importStar(require("vscode"));
const model_js_1 = require("./model.js");
const tree_js_1 = require("./tree.js");
const ICONS = { "memory.pending": "lightbulb", "memory.approved": "pass", "memory.aside": "circle-slash" };
function sidebarMemories(context, engine) {
    const tree = new tree_js_1.RowTree("crazyagent.memories", (row, item) => {
        item.iconPath = new vscode.ThemeIcon(ICONS[row.kind] ?? "circle-outline");
        item.tooltip = row.memory?.text;
    });
    const load = async () => {
        const memories = [];
        let after = null;
        for (let pages = 1;; pages += 1) {
            const reply = await (0, tree_js_1.ask)(engine, "ide.memories", after === null ? {} : { after });
            if (reply?.kind !== "ok") {
                tree.say((0, tree_js_1.trouble)(reply));
                return;
            }
            memories.push(...reply.data);
            after = (0, model_js_1.nextPage)(reply.data, pages);
            if (after === null)
                break;
        }
        tree.say(memories.length === 0 ? "No memories yet." : undefined);
        tree.show((0, model_js_1.memoryGroups)(memories));
    };
    const refresh = (0, tree_js_1.coalesced)(load);
    const decide = (decision) => async (row) => {
        const memory = row?.memory;
        if (memory === undefined || memory.state !== "pending")
            return;
        const reply = await (0, tree_js_1.ask)(engine, "ide.memory_decide", { id: memory.id, revision: memory.revision, decision });
        if (reply?.kind !== "ok")
            void vscode.window.showWarningMessage((0, tree_js_1.trouble)(reply));
        else
            void vscode.window.showInformationMessage(`Confirm in the CrazyAgent window to ${decision === "approve" ? "save" : "reject"} this memory.`);
        refresh();
    };
    context.subscriptions.push(tree, engine.on("memories_changed", refresh), vscode.commands.registerCommand("crazyagent.memories.approve", decide("approve")), vscode.commands.registerCommand("crazyagent.memories.reject", decide("reject")), vscode.commands.registerCommand("crazyagent.memories.refresh", refresh));
    refresh();
}
