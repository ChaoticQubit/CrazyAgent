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
exports.registerBaseContent = registerBaseContent;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("./scm-model.js");
const PAGES_MAX = 512;
const SIDES = [
    [scm_model_js_1.BASE_SCHEME, "ide.base_blob"],
    [scm_model_js_1.CONFIG_SCHEME, "ide.config_blob"],
    [scm_model_js_1.HEAD_SCHEME, "ide.head_blob"],
];
function registerBaseContent(context, engine) {
    for (const [scheme, call] of SIDES) {
        context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(scheme, provider(engine, call)));
    }
}
function provider(engine, call) {
    return {
        async provideTextDocumentContent(uri) {
            const target = (0, scm_model_js_1.parseBasePath)(uri.path);
            if (target === null)
                throw new Error((0, scm_model_js_1.refusal)("changes_not_found"));
            const parts = [];
            let page = 0;
            for (;;) {
                if (parts.length >= PAGES_MAX)
                    throw new Error((0, scm_model_js_1.refusal)("changes_too_large"));
                const reply = await engine.request(call, { id: target.id, head: uri.query, path: target.path, page });
                if (reply.kind !== "ok")
                    throw new Error((0, scm_model_js_1.refusal)(reply.code));
                const data = reply.data ?? {};
                if (typeof data.text !== "string")
                    throw new Error((0, scm_model_js_1.refusal)("changes_outcome_unknown"));
                parts.push(data.text);
                if (data.next === null)
                    return parts.join("");
                if (typeof data.next !== "number" || !Number.isInteger(data.next) || data.next <= page) {
                    throw new Error((0, scm_model_js_1.refusal)("changes_outcome_unknown"));
                }
                page = data.next;
            }
        },
    };
}
