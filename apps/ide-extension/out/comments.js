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
exports.registerComments = registerComments;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("./scm-model.js");
const CONTROLLER = "crazyagent.comments";
const FRESH_MS = 5_000;
function registerComments(context, engine) {
    const controller = vscode.comments.createCommentController(CONTROLLER, "CrazyAgent comments");
    controller.options = { prompt: "Comment for the agent", placeHolder: "What should the agent change here?" };
    let sets = [];
    let loaded = 0;
    let loading;
    const load = () => {
        loading ??= engine
            .request("ide.change_sets", {})
            .then((reply) => {
            sets = reply.kind === "ok" && Array.isArray(reply.data) ? reply.data : [];
        }, () => {
            sets = [];
        })
            .finally(() => {
            loaded = Date.now();
            loading = undefined;
        });
        return loading;
    };
    const fresh = async () => {
        if (Date.now() - loaded > FRESH_MS)
            await load();
    };
    const targetOf = (uri) => {
        if (uri.scheme === scm_model_js_1.HEAD_SCHEME)
            return (0, scm_model_js_1.headCommentTarget)(sets, uri.path);
        return uri.scheme === "file" ? (0, scm_model_js_1.commentTarget)(sets, uri.fsPath) : null;
    };
    controller.commentingRangeProvider = {
        provideCommentingRanges: async (document) => {
            if (document.uri.scheme !== "file" && document.uri.scheme !== scm_model_js_1.HEAD_SCHEME)
                return [];
            await fresh();
            if (targetOf(document.uri) === null)
                return [];
            return [new vscode.Range(0, 0, Math.max(document.lineCount - 1, 0), 0)];
        },
    };
    const send = async (reply) => {
        const { thread, text } = reply;
        await fresh();
        const target = targetOf(thread.uri);
        if (target === null) {
            void vscode.window.showErrorMessage((0, scm_model_js_1.refusal)("changes_not_found"));
            return;
        }
        if ((0, scm_model_js_1.commentTooLong)(text)) {
            void vscode.window.showErrorMessage((0, scm_model_js_1.commentRefusal)("ide_comment_too_long"));
            return;
        }
        const line = (thread.range?.start.line ?? 0) + 1;
        let answer;
        try {
            answer = await engine.request("ide.comment", { id: target.id, path: target.path, line, text });
        }
        catch {
            void vscode.window.showErrorMessage((0, scm_model_js_1.refusal)("changes_outcome_unknown"));
            return;
        }
        if (answer.kind !== "ok") {
            void vscode.window.showErrorMessage((0, scm_model_js_1.commentRefusal)(answer.code));
            return;
        }
        const body = new vscode.MarkdownString();
        body.appendText(text);
        thread.comments = [...thread.comments, { body, mode: vscode.CommentMode.Preview, author: { name: "You" }, label: "Sent" }];
        thread.canReply = false;
        void vscode.window.showInformationMessage((0, scm_model_js_1.commentSent)(target.agent));
    };
    context.subscriptions.push(controller, engine.on("change_sets_changed", () => {
        loaded = 0;
    }), vscode.commands.registerCommand("crazyagent.comments.send", send));
}
