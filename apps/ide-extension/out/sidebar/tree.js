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
exports.RowTree = void 0;
exports.coalesced = coalesced;
exports.trouble = trouble;
exports.ask = ask;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("../scm-model.js");
class RowTree {
    view;
    #changed = new vscode.EventEmitter();
    onDidChangeTreeData = this.#changed.event;
    #decorate;
    #rows = [];
    constructor(id, decorate) {
        this.#decorate = decorate;
        this.view = vscode.window.createTreeView(id, { treeDataProvider: this });
    }
    show(rows) {
        this.#rows = rows;
        this.#changed.fire();
    }
    say(message) {
        this.view.message = message;
    }
    getChildren(row) {
        return row === undefined ? this.#rows : row.children;
    }
    getTreeItem(row) {
        const state = row.kind === "group" ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None;
        const item = new vscode.TreeItem(row.label, state);
        item.id = row.key;
        item.description = row.description;
        item.contextValue = row.kind;
        if (row.kind !== "group")
            this.#decorate(row, item);
        return item;
    }
    dispose() {
        this.view.dispose();
        this.#changed.dispose();
    }
}
exports.RowTree = RowTree;
function coalesced(load) {
    let running = false;
    let again = false;
    const refresh = () => {
        if (running) {
            again = true;
            return;
        }
        running = true;
        again = false;
        const done = () => {
            running = false;
            if (again)
                refresh();
        };
        load().then(done, done);
    };
    return refresh;
}
function trouble(reply) {
    if (reply === null)
        return "CrazyAgent is not reachable. It will try again on the next change.";
    if (reply.kind === "stale")
        return "This changed in CrazyAgent. The list now shows the current version.";
    if (reply.kind === "rejected")
        return (0, scm_model_js_1.unconfirmed)(reply.code) ?? `CrazyAgent refused this (${reply.code}).`;
    return "";
}
async function ask(engine, name, args = {}) {
    try {
        return await engine.request(name, args);
    }
    catch {
        return null;
    }
}
