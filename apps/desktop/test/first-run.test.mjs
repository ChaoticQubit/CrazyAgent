import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { validateProfile, emptyProfileDraft, draftFromProfile, Welcome } = await import("../src/screens/first-run/index.tsx");

const draft = (patch = {}) => ({ ...emptyProfileDraft(), name: "Mira Okafor", ...patch });

test("a valid profile is trimmed into the engine shape with the default working style", () => {
  const result = validateProfile(draft({ name: "  Mira Okafor ", role: " Founder ", bio: "Builds tools.\nLikes tests.", stack: ["Rust", "React"] }));
  assert.deepEqual(result, { profile: {
    name: "Mira Okafor", role: "Founder", bio: "Builds tools.\nLikes tests.", preferred_stack: ["Rust", "React"],
    working_style: { detail: "terse", initiative: "ask_first" },
  } });
  assert.deepEqual(emptyProfileDraft(), { name: "", role: "", bio: "", stack: [], detail: "terse", initiative: "ask_first" });
});

test("profile validation names each wrong field in plain words and keeps every value", () => {
  assert.deepEqual(validateProfile(draft({ name: "   " })), { errors: { name: "Enter your name." } });
  assert.deepEqual(validateProfile(draft({ name: "x".repeat(81) })), { errors: { name: "Keep your name under 81 characters." } });
  assert.deepEqual(validateProfile(draft({ name: "Mira\nOkafor" })), { errors: { name: "Keep your name on one line." } });
  assert.deepEqual(validateProfile(draft({ role: "a\u0000b" })), { errors: { role: "Keep your role on one line." } });
  assert.deepEqual(validateProfile(draft({ role: "r".repeat(81) })), { errors: { role: "Keep your role under 81 characters." } });
  assert.deepEqual(validateProfile(draft({ bio: "b".repeat(501) })), { errors: { bio: "Keep your bio under 501 characters." } });
  assert.deepEqual(validateProfile(draft({ bio: "ok\u0007" })), { errors: { bio: "Remove special characters from your bio." } });
  assert.deepEqual(validateProfile(draft({ stack: Array.from({ length: 13 }, (_, index) => `T${index}`) })), { errors: { stack: "List up to 12 technologies." } });
  assert.deepEqual(validateProfile(draft({ stack: ["x".repeat(41)] })), { errors: { stack: "Keep each technology under 41 characters." } });
  assert.deepEqual(validateProfile(draft({ detail: "loud" })), { errors: { style: "Choose how agents reply and act." } });
  assert.equal([..."é".repeat(80)].length, 80);
  assert.ok("profile" in validateProfile(draft({ name: "é".repeat(80) })));
});

test("an engine profile loads back into the same draft", () => {
  const profile = { name: "Mira", role: "Designer", bio: "", preferred_stack: ["Figma"], working_style: { detail: "detailed", initiative: "act_first" } };
  assert.deepEqual(draftFromProfile(profile), { name: "Mira", role: "Designer", bio: "", stack: ["Figma"], detail: "detailed", initiative: "act_first" });
  assert.deepEqual(validateProfile(draftFromProfile(profile)), { profile });
});

test("Welcome renders the first-run form with real labels, defaults and no sidebar", () => {
  const markup = renderToStaticMarkup(createElement(Welcome, { onSaved: () => {} }));
  assert.match(markup, /CrazyAgent/);
  assert.match(markup, /<h1>Tell CrazyAgent about you<\/h1>/);
  for (const [id, label] of [["profile-name", "Name"], ["profile-role", "Role"], ["profile-bio", "Short bio"], ["profile-stack", "Preferred stack"]]) {
    assert.match(markup, new RegExp(`<label for="${id}">${label}</label>`));
  }
  assert.match(markup, /<input[^>]*id="profile-name"[^>]*maxLength="80"/);
  assert.match(markup, /<textarea[^>]*id="profile-bio"[^>]*maxLength="500"/);
  assert.match(markup, /role="radiogroup" aria-label="Replies"/);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Short</);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Ask first</);
  assert.match(markup, /<button type="submit" class="btn primary large"[^>]*>Save and continue<\/button>/);
  assert.doesNotMatch(markup, /class="sidebar"/);
  assert.doesNotMatch(markup, /role="alert"/);
});
