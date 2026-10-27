"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const engine_js_1 = require("./engine.js");
const reveal_js_1 = require("./reveal.js");
const features = [reveal_js_1.reveal];
function activate(context) {
    const engine = engine_js_1.Engine.fromEnv(process.env);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
