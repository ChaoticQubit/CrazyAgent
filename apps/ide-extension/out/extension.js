"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const base_content_js_1 = require("./base-content.js");
const comments_js_1 = require("./comments.js");
const engine_js_1 = require("./engine.js");
const scm_js_1 = require("./scm.js");
const trust_js_1 = require("./trust.js");
const features = [base_content_js_1.registerBaseContent, scm_js_1.registerChanges, comments_js_1.registerComments, trust_js_1.registerTrust];
function activate(context) {
    const engine = engine_js_1.Engine.fromEnv(process.env);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
