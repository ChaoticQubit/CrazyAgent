import "../test/load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { methodOptions, TILE_LINES } = await import("../src/screens/settings/add-account.tsx");
const { SignInPanel } = await import("../src/screens/settings/sign-in.tsx");

const field = (url, key, model) => ({ url, key, model });
const chatgpt = { provider: "chatgpt", name: "ChatGPT / Codex", methods: [
  { method: "subscription", label: "Sign in with ChatGPT", url_preset: null, ...field("hidden", "hidden", "hidden") },
  { method: "api_key", label: "Use an API key", url_preset: null, ...field("hidden", "required", "optional") }] };
const claude = { provider: "claude", name: "Claude", methods: [
  { method: "subscription", label: "Sign in with your Claude subscription", url_preset: null, ...field("hidden", "hidden", "hidden") },
  { method: "api_key", label: "Use an API key", url_preset: null, ...field("hidden", "required", "optional") }] };
const local = { provider: "local", name: "Local models (Ollama)", methods: [
  { method: "url", label: "Connect a URL", url_preset: "http://127.0.0.1:11434/v1", ...field("optional", "hidden", "required") }] };
const forbidden = () => { throw new Error("static rendering must not start, cancel or send anything"); };

test("managed sign-in comes first and its start shows no routing fields, password field or unseen sign-in", () => {
  for (const provider of [chatgpt, claude]) {
    const [first] = methodOptions(provider);
    assert.equal(first.entry.method, "subscription", provider.provider);
    assert.equal(first.recommended, true, provider.provider);
    assert.ok(first.native, `${provider.provider} starts the managed sign-in`);
  }
  for (const [provider, method] of [["chatgpt", "codex_device_code"], ["chatgpt", "codex_existing"], ["claude", "claude_official"]]) {
    const markup = renderToStaticMarkup(createElement(SignInPanel, { provider, method, state: undefined, idle: true,
      onStart: forbidden, onCancel: forbidden, onCheck: forbidden, onClaudeCode: forbidden }));
    assert.doesNotMatch(markup, /(?:Harness reference|Protocol reference|Profile reference|Account reference|Provider reference|Model author reference|Endpoint URL)/, method);
    assert.doesNotMatch(markup, /type="password"/, method);
    assert.doesNotMatch(markup, /(?:Signed in as|Account authenticated|Model ready)/, method);
    assert.ok(Buffer.byteLength(markup, "utf8") < 65536);
  }
});

test("saved-key and compatible endpoint setup sit beside the managed sign-in", () => {
  const options = methodOptions(claude);
  const key = options.find((option) => option.entry.method === "api_key");
  assert.ok(key, "a managed provider also offers its API key");
  assert.equal(key.native, undefined);
  assert.equal(key.recommended, false);
  assert.match(TILE_LINES.openai_compatible, /OpenAI API/);
  assert.match(TILE_LINES.local, /Ollama/);
  const [url] = methodOptions(local);
  assert.equal(url.entry.method, "url");
  assert.equal(url.native, undefined);
});
