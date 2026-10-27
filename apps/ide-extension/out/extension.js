"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const base_content_js_1 = require("./base-content.js");
const engine_js_1 = require("./engine.js");
const scm_js_1 = require("./scm.js");
const features = [base_content_js_1.registerBaseContent, scm_js_1.registerChanges];
function activate(context) {
    const engine = engine_js_1.Engine.fromEnv(process.env);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
