import "../test/load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { KeyOrUrlForm, methodOptions } = await import("../src/screens/settings/add-account.tsx");
const { SessionContext, useSession } = await import("../src/state.ts");

const field = (url, key, model) => ({ url, key, model });
const compatible = { provider: "openai_compatible", name: "OpenAI-compatible", methods: [
  { method: "url", label: "Connect a URL", url_preset: null, ...field("required", "optional", "required") }] };
const openrouter = { provider: "openrouter", name: "OpenRouter", methods: [
  { method: "api_key", label: "Use an API key", url_preset: null, ...field("hidden", "required", "required") }] };
const forbidden = () => { throw new Error("static rendering must not save anything"); };

function Form({ provider }) {
  const [option] = methodOptions(provider);
  return createElement(SessionContext.Provider, { value: useSession() },
    createElement(KeyOrUrlForm, { provider, option, name: "Account 1", onName: forbidden, onAdded: forbidden }));
}

test("ordinary URL account fields carry endpoint and model without routing identities", () => {
  const markup = renderToStaticMarkup(createElement(Form, { provider: compatible }));
  assert.match(markup, /<label for="account-url">Base URL<\/label>/);
  const url = markup.match(/<input\b[^>]*id="account-url"[^>]*>/)?.[0];
  assert.ok(url, "the base URL has its own field");
  assert.match(url, /type="url"/);
  assert.match(markup, /<label for="account-model">Model<\/label>/);
  assert.match(markup, /API key \(optional\)/);
  assert.doesNotMatch(markup, /(?:Harness reference|Profile reference|Credential reference|Account reference)/);
  assert.doesNotMatch(markup, /(?:Authenticated|Model ready|setup unavailable)/);
});

test("the key field is masked, uncontrolled, bounded and never shown again", () => {
  const markup = renderToStaticMarkup(createElement(Form, { provider: openrouter }));
  assert.doesNotMatch(markup, /id="account-url"/);
  const input = markup.match(/<input\b[^>]*id="account-key"[^>]*>/)?.[0];
  assert.ok(input, "the key has its own field");
  assert.match(input, /type="password"/);
  assert.match(input, /maxLength="8192"/i);
  assert.match(input, /autoComplete="off"/i);
  assert.doesNotMatch(input, /\b(?:value|name)=/);
  assert.match(markup, /Sent once to CrazyAgent&#x27;s engine, which keeps it privately\. You won&#x27;t see it again\./);
  assert.match(markup, />Save key<\/button>/);
  assert.doesNotMatch(markup, /(?:Authenticated|Model ready)/);
});
