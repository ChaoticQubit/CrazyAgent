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
exports.LOCKED_ENV = void 0;
exports.startLock = startLock;
const node_fs_1 = require("node:fs");
const vscode = __importStar(require("vscode"));
const lock_model_js_1 = require("./lock-model.js");
exports.LOCKED_ENV = "CRAZYAGENT_IDE_LOCKED_FILE";
function startLock(context, env) {
    const path = env[exports.LOCKED_ENV];
    if (path === undefined || path === "")
        return;
    let read;
    try {
        read = (0, node_fs_1.statSync)(path).size > lock_model_js_1.LOCKED_BYTES_MAX ? undefined : (0, lock_model_js_1.parseLocked)((0, node_fs_1.readFileSync)(path, "utf8"));
    }
    catch {
        return;
    }
    if (read === undefined)
        return;
    const locked = read;
    const keys = Object.keys(locked);
    const reassert = () => {
        const config = vscode.workspace.getConfiguration();
        const seen = {};
        for (const key of keys)
            seen[key] = { value: config.get(key), user: config.inspect(key)?.globalValue };
        for (const { key, value } of (0, lock_model_js_1.restores)(locked, seen)) {
            Promise.resolve(config.update(key, value, vscode.ConfigurationTarget.Global)).catch(() => undefined);
        }
    };
    reassert();
    context.subscriptions.push(vscode.workspace.onDidChangeConfiguration((event) => {
        if (keys.some((key) => event.affectsConfiguration(key)))
            reassert();
    }));
}
