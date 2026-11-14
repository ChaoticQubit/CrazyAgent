import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { validateProfile, emptyProfileDraft, draftFromProfile, Welcome, skipGuideNote, ProfileFields } = await import("../src/screens/first-run/index.tsx");
const { plainError } = await import("../src/api.ts");
const { errorMessage } = await import("../src/engine-client.ts");

const NAME_WORDS = "Enter your name on one line, up to 100 bytes (about 100 English letters or 33 Chinese, Hindi or Thai ones), with no hidden characters.";
const ROLE_WORDS = "Keep your role on one line, up to 100 bytes (about 100 English letters or 33 Chinese, Hindi or Thai ones), with no hidden characters.";
const BIO_WORDS = "Keep your bio to 1000 bytes (about 1000 English letters or 333 Chinese, Hindi or Thai ones), with no tabs, special or hidden characters.";
const STACK_WORDS = "List up to 12 different technologies, each on one line, up to 40 bytes, with no hidden characters.";

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
  assert.deepEqual(validateProfile(draft({ name: "x".repeat(101) })), { errors: { name: NAME_WORDS } });
  assert.deepEqual(validateProfile(draft({ name: "Mira\nOkafor" })), { errors: { name: NAME_WORDS } });
  assert.deepEqual(validateProfile(draft({ role: "a\u0000b" })), { errors: { role: ROLE_WORDS } });
  assert.deepEqual(validateProfile(draft({ role: "r".repeat(101) })), { errors: { role: ROLE_WORDS } });
  assert.deepEqual(validateProfile(draft({ bio: "b".repeat(1001) })), { errors: { bio: BIO_WORDS } });
  assert.deepEqual(validateProfile(draft({ bio: "ok\u0007" })), { errors: { bio: BIO_WORDS } });
  assert.deepEqual(validateProfile(draft({ stack: Array.from({ length: 13 }, (_, index) => `T${index}`) })), { errors: { stack: STACK_WORDS } });
  assert.deepEqual(validateProfile(draft({ stack: ["x".repeat(41)] })), { errors: { stack: STACK_WORDS } });
  assert.deepEqual(validateProfile(draft({ detail: "loud" })), { errors: { style: "Choose how agents reply and act." } });
  for (const valid of [{ name: "x".repeat(100) }, { role: "r".repeat(100) }, { bio: "b".repeat(1000) }, { stack: ["x".repeat(40)] }, { name: "é".repeat(50) }]) {
    assert.ok("profile" in validateProfile(draft(valid)), JSON.stringify(valid));
  }
});

test("the app measures the profile in UTF-8 bytes and refuses tabs in the bio, as the engine does", () => {
  assert.deepEqual(validateProfile(draft({ name: "é".repeat(51) })), { errors: { name: NAME_WORDS } });
  assert.ok("profile" in validateProfile(draft({ bio: "\u754c".repeat(333) })));
  assert.deepEqual(validateProfile(draft({ bio: "\u754c".repeat(334) })), { errors: { bio: BIO_WORDS } });
  assert.deepEqual(validateProfile(draft({ bio: "one\ttwo" })), { errors: { bio: BIO_WORDS } });
  assert.deepEqual(validateProfile(draft({ stack: ["\u754c".repeat(14)] })), { errors: { stack: STACK_WORDS } });
});

test("hidden, direction and tag characters are refused in every profile field, an emoji joiner is not", () => {
  assert.deepEqual(validateProfile(draft({ name: "Mira\u200b" })), { errors: { name: NAME_WORDS } });
  assert.deepEqual(validateProfile(draft({ role: "\u202eFounder" })), { errors: { role: ROLE_WORDS } });
  assert.deepEqual(validateProfile(draft({ bio: "Builds tools.\u{e0041}\u{e0042}" })), { errors: { bio: BIO_WORDS } });
  assert.deepEqual(validateProfile(draft({ bio: "Builds\u2066 tools." })), { errors: { bio: BIO_WORDS } });
  assert.deepEqual(validateProfile(draft({ stack: ["Ru\ufeffst"] })), { errors: { stack: STACK_WORDS } });
  assert.ok("profile" in validateProfile(draft({ bio: "\u{1f469}\u200d\u{1f4bb} builds tools." })));
});

test("the engine refusals read in the same words on the profile form, the app copy and the engine client", () => {
  for (const [code, words] of [["owner_profile_name", NAME_WORDS], ["owner_profile_role", ROLE_WORDS], ["owner_profile_bio", BIO_WORDS], ["owner_profile_stack", STACK_WORDS]]) {
    assert.equal(plainError(code), words, code);
    assert.equal(errorMessage({ code }), words, code);
  }
});

test("the bio counter counts UTF-8 bytes against the engine's 1000", () => {
  const markup = renderToStaticMarkup(createElement(ProfileFields, { draft: draft({ bio: "\u754c".repeat(300) }), onDraft: () => {}, errors: {} }));
  assert.match(markup, /900 of 1,000 bytes/);
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
  assert.match(markup, /<input[^>]*id="profile-name"[^>]*maxLength="100"/);
  assert.match(markup, /<textarea[^>]*id="profile-bio"[^>]*maxLength="1000"/);
  assert.match(markup, /role="radiogroup" aria-label="Replies"/);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Short</);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Ask first</);
  assert.match(markup, /<button type="submit" class="btn primary large"[^>]*>Save and continue<\/button>/);
  assert.doesNotMatch(markup, /class="sidebar"/);
  assert.doesNotMatch(markup, /role="alert"/);
});

test("the last first-run page offers Learn CrazyAgent, ticked, under Add account and Skip for now", () => {
  const markup = renderToStaticMarkup(createElement(Welcome, { onSaved: () => {}, initialStep: "accounts" }));
  assert.match(markup, /<h1>Add an account<\/h1>/);
  assert.match(markup, /<p>Agents need a model account to work\. Sign in with ChatGPT or Claude, or add an API key or a local model\.<\/p>/);
  const add = markup.indexOf("Add account</button>");
  const skip = markup.indexOf("Skip for now</button>");
  const offer = markup.indexOf('<h2 id="learn-offer-title">Learn CrazyAgent</h2>');
  assert.ok(add > 0 && skip > add && offer > skip, markup);
  assert.match(markup, /aria-labelledby="learn-offer-title"/);
  assert.match(markup, /<p>A short guide on Home walks you through accounts, projects, the master and its agents, questions and change sets\. Its first step is an optional demo project\.<\/p>/);
  assert.match(markup, /<label class="checkbox"><input type="checkbox" checked=""\/><span>Show the guide on Home<\/span><\/label>/);
  assert.doesNotMatch(markup, /role="alert"/);
});

test("skipping the guide says nothing when saved and says it stays on Home when it did not save", () => {
  assert.equal(skipGuideNote({ kind: "ok", value: undefined }), undefined);
  assert.equal(skipGuideNote({ kind: "not_ready" }), "Skipping the guide isn't ready in this build, so it stays on Home.");
  assert.equal(skipGuideNote({ kind: "error", code: "uncertain", message: "CrazyAgent did not answer in time. Try again." }),
    "Couldn't skip the guide, so it stays on Home. CrazyAgent did not answer in time. Try again.");
});
