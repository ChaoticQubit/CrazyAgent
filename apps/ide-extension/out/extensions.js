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
exports.extensions = void 0;
exports.parseRequest = parseRequest;
exports.answerLine = answerLine;
const vscode = __importStar(require("vscode"));
const scm_model_js_1 = require("./scm-model.js");
const REQUEST = "crazyagent.extensions.request";
const UPDATES = "crazyagent.extensions.updates";
const ASK_UPDATE = "Ask to update";
const DISMISS = "Dismiss";
function parseRequest(text) {
    const [id = "", version = "latest", ...rest] = text.trim().split("@");
    const named = /^[A-Za-z0-9_-]{1,128}\.[A-Za-z0-9_-]{1,128}$/.test(id);
    const versioned = /^[A-Za-z0-9+-][A-Za-z0-9.+-]{0,63}$/.test(version);
    if (rest.length > 0 || !named || !versioned)
        return undefined;
    return { id: id.toLowerCase(), version };
}
function answerLine(reply) {
    if (reply.kind !== "ok")
        return (0, scm_model_js_1.unconfirmed)(reply.code) ?? `CrazyAgent refused this (${reply.code}).`;
    if (reply.data.state === "approved")
        return "This version is approved. The IDE installs it when it next starts.";
    if (reply.data.state === "dismissed")
        return "Dismissed. It shows again if a newer version comes out.";
    return "Confirm this in the CrazyAgent app. Nothing installs until you approve it there.";
}
async function show(pending) {
    try {
        void vscode.window.showInformationMessage(answerLine(await pending));
    }
    catch (error) {
        void vscode.window.showErrorMessage(`CrazyAgent didn't answer (${error.message}).`);
    }
}
async function request(engine) {
    const text = await vscode.window.showInputBox({
        title: "Install an extension from Open VSX",
        prompt: "publisher.name for the newest version, or publisher.name@version for an exact one",
        validateInput: (value) => (parseRequest(value) ? undefined : "Type publisher.name or publisher.name@version."),
    });
    const wanted = text === undefined ? undefined : parseRequest(text);
    if (wanted === undefined)
        return;
    await show(engine.request("ide.extension_request", { id: wanted.id, version: wanted.version }));
}
async function listing(engine) {
    try {
        const reply = await engine.request("ide.extensions");
        return reply.kind === "ok" ? reply.data : undefined;
    }
    catch {
        return undefined;
    }
}
async function updates(engine) {
    const listed = await listing(engine);
    if (listed === undefined) {
        void vscode.window.showErrorMessage("CrazyAgent couldn't read the extension updates.");
        return;
    }
    if (listed.updates.length === 0) {
        void vscode.window.showInformationMessage("No extension updates are available.");
        return;
    }
    const picked = await vscode.window.showQuickPick(listed.updates.map((update) => ({
        label: update.id,
        description: `${update.current ?? "not approved"} to ${update.version}`,
        update,
    })), { title: "Extension updates", placeHolder: "Nothing updates until the owner approves it in the CrazyAgent app" });
    if (picked === undefined)
        return;
    const choice = await vscode.window.showQuickPick([ASK_UPDATE, DISMISS], {
        title: `${picked.update.id} ${picked.update.version}`,
    });
    if (choice === undefined)
        return;
    const { id, version } = picked.update;
    await show(engine.request("ide.extension_decide", { id, version, approve: choice === ASK_UPDATE }));
}
async function announce(engine) {
    const listed = await listing(engine);
    const count = listed?.updates.length ?? 0;
    if (count === 0)
        return;
    const noun = count === 1 ? "update is" : "updates are";
    const open = await vscode.window.showInformationMessage(`${count} extension ${noun} available. Nothing updates until you approve it.`, "Show updates");
    if (open !== undefined)
        await updates(engine);
}
const extensions = (context, engine) => {
    context.subscriptions.push(vscode.commands.registerCommand(REQUEST, () => request(engine)), vscode.commands.registerCommand(UPDATES, () => updates(engine)));
    void announce(engine);
};
exports.extensions = extensions;
