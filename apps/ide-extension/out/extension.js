"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const engine_js_1 = require("./engine.js");
const extensions_js_1 = require("./extensions.js");
const features = [extensions_js_1.extensions];
function activate(context) {
    const engine = engine_js_1.Engine.fromEnv(process.env);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
