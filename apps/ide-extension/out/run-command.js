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
exports.TERMINAL_NAME = void 0;
exports.runCommand = runCommand;
const vscode = __importStar(require("vscode"));
const run_command_model_js_1 = require("./run-command-model.js");
const terminal_js_1 = require("./terminal.js");
exports.TERMINAL_NAME = "Overview command";
function runCommand(context, engine) {
    context.subscriptions.push(engine.on("run_command", (event) => {
        (0, run_command_model_js_1.handle)(event, {
            open: () => open(engine),
            ack: (request) => engine.request("ide.run_ack", { request }),
        }).then(undefined, () => undefined);
    }));
}
function open(engine) {
    const level = (0, terminal_js_1.defaultLevel)();
    if (level === "full") {
        return { terminal: vscode.window.createTerminal({ name: exports.TERMINAL_NAME }), ready: Promise.resolve(true) };
    }
    const options = (0, terminal_js_1.engineTerminal)(engine, exports.TERMINAL_NAME, level);
    return { terminal: vscode.window.createTerminal(options), ready: options.pty.ready };
}
