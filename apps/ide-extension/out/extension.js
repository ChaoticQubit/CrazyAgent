"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const engine_js_1 = require("./engine.js");
const lock_js_1 = require("./lock.js");
const features = [];
function activate(context) {
    (0, lock_js_1.startLock)(context, process.env);
    const engine = engine_js_1.Engine.fromEnv(process.env);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
