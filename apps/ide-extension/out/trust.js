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
exports.registerTrust = registerTrust;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("./scm-model.js");
const trust_model_js_1 = require("./trust-model.js");
const POLL_MS = 5_000;
const BADGE = "!";
function registerTrust(context, engine) {
    const changed = new vscode.EventEmitter();
    const warned = new Set();
    let files = new Map();
    let loading;
    const show = (sets) => {
        files = (0, trust_model_js_1.ideConfigFiles)(sets);
        changed.fire(undefined);
        for (const set of sets) {
            const warning = (0, trust_model_js_1.ideConfigWarning)(set);
            if (warning === null || warned.has((0, trust_model_js_1.warningKey)(set)))
                continue;
            warned.add((0, trust_model_js_1.warningKey)(set));
            void vscode.window.showWarningMessage(warning);
        }
    };
    const load = () => {
        loading ??= engine
            .request("ide.change_sets", {})
            .then((reply) => {
            if (reply.kind === "ok" && Array.isArray(reply.data))
                show(reply.data);
        }, () => undefined)
            .finally(() => {
            loading = undefined;
        });
        return loading;
    };
    const flagged = (tooltip, propagate) => {
        const decoration = new vscode.FileDecoration(BADGE, tooltip, new vscode.ThemeColor("list.warningForeground"));
        decoration.propagate = propagate;
        return decoration;
    };
    const provider = {
        onDidChangeFileDecorations: changed.event,
        provideFileDecoration: (uri) => {
            if (uri.scheme === scm_model_js_1.CONFIG_SCHEME)
                return flagged(trust_model_js_1.IDE_CONFIG_NOTE, false);
            if (uri.scheme !== "file")
                return undefined;
            const agent = files.get(uri.fsPath);
            return agent === undefined ? undefined : flagged(`${trust_model_js_1.IDE_CONFIG_NOTE} (${agent})`, true);
        },
    };
    const timer = setInterval(() => void load(), POLL_MS);
    context.subscriptions.push(changed, vscode.window.registerFileDecorationProvider(provider), engine.on("change_sets_changed", () => void load()), { dispose: () => clearInterval(timer) });
    void load();
}
