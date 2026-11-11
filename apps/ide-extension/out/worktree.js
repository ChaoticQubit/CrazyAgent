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
exports.EDIT_ANYWAY = void 0;
exports.agentOf = agentOf;
exports.insideFolder = insideFolder;
exports.editAnywayQuestion = editAnywayQuestion;
const path = __importStar(require("node:path"));
exports.EDIT_ANYWAY = "Edit anyway";
function agentOf(folder) {
    const match = /^agent-([1-9][0-9]*)$/.exec(path.basename(folder));
    if (match === null || path.basename(path.dirname(path.resolve(folder))) !== "worktrees")
        return null;
    return `team-${match[1]}`;
}
function insideFolder(folder, file) {
    const relative = path.relative(folder, file);
    if (relative === "" || path.isAbsolute(relative) || relative.split(path.sep).includes(".."))
        return null;
    return relative.split(path.sep).join("/");
}
function editAnywayQuestion(name) {
    return `${name} is running in this worktree. Edit anyway?`;
}
