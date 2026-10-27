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
exports.sidebarChanges = sidebarChanges;
const vscode = __importStar(require("vscode"));
const model_js_1 = require("./model.js");
const tree_js_1 = require("./tree.js");
function sidebarChanges(context, engine) {
    const tree = new tree_js_1.RowTree("crazyagent.changeSets", (_row, item) => {
        item.iconPath = new vscode.ThemeIcon("git-pull-request");
        item.tooltip = "Open in Source Control";
        item.command = { command: "workbench.view.scm", title: "Open in Source Control" };
    });
    const load = async () => {
        const reply = await (0, tree_js_1.ask)(engine, "ide.change_sets");
        if (reply?.kind !== "ok") {
            tree.say((0, tree_js_1.trouble)(reply));
            return;
        }
        const rows = (0, model_js_1.changeGroups)(reply.data, Date.now());
        tree.say(rows.length === 0 ? "No change sets waiting." : undefined);
        tree.show(rows);
    };
    const refresh = (0, tree_js_1.coalesced)(load);
    context.subscriptions.push(tree, engine.on("change_sets_changed", refresh), vscode.commands.registerCommand("crazyagent.changeSets.refresh", refresh));
    refresh();
}
