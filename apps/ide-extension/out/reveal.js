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
exports.reveal = reveal;
const vscode = __importStar(require("vscode"));
const reveal_path_js_1 = require("./reveal-path.js");
function reveal(context, engine) {
    context.subscriptions.push(engine.on("reveal", (event) => {
        show(event.path).then((shown) => (shown ? engine.request("ide.reveal_ack", { request: event.request }) : undefined), () => undefined);
    }));
}
async function show(path) {
    const folders = vscode.workspace.workspaceFolders ?? [];
    const place = (0, reveal_path_js_1.revealPlace)(folders.map((folder) => folder.uri.path), path);
    if (place === undefined)
        return false;
    const folder = folders[place.index].uri;
    const uri = place.parts.length === 0 ? folder : vscode.Uri.joinPath(folder, ...place.parts);
    const stat = await vscode.workspace.fs.stat(uri);
    if (stat.type & vscode.FileType.Directory) {
        await vscode.commands.executeCommand("revealInExplorer", uri);
    }
    else {
        await vscode.window.showTextDocument(uri, { preview: false });
    }
    return true;
}
