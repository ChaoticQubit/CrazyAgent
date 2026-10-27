"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const engine_js_1 = require("./engine.js");
const agents_js_1 = require("./sidebar/agents.js");
const changes_js_1 = require("./sidebar/changes.js");
const context_js_1 = require("./sidebar/context.js");
const memories_js_1 = require("./sidebar/memories.js");
const features = [
    memories_js_1.sidebarMemories,
    agents_js_1.sidebarAgents,
    changes_js_1.sidebarChanges,
    context_js_1.sidebarContext,
];
function activate(context) {
    const engine = engine_js_1.Engine.fromEnv(process.env);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
