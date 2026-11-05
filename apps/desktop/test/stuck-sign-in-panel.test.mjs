import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";
import "./load-source.mjs";

const api = await import("../src/api.ts");
const { STUCK_SIGN_IN_CODES } = await import("../src/state.ts");

const fixture = await bundle(new URL("./fixtures/sign-in-noted.mjs", import.meta.url).pathname);

for (const code of ["native_auth_recovery_required", "native_auth_outcome_unknown_read_same_operation"]) {
  test(`a Sign in again the bridge refuses with ${code} reaches the session, so the window can raise Clear stuck sign-in`, async () => {
    const seen = await withPage(async ({ evaluate }) => {
      await evaluate(fixture);
      await evaluate(`Fixture.mount(${JSON.stringify(code)})`);
      return evaluate("Fixture.startSignIn()");
    });
    assert.equal(seen.offered, true, seen.text);
    assert.deepEqual(seen.noted.filter((noted) => noted === code), [code], `${JSON.stringify(seen.noted)}\n${seen.text}`);
  });
}

test("a Sign in again refused because another account's sign-in still runs names that account and offers no Clear stuck sign-in", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate('Fixture.mount("native_auth_sign_in_running", "acct-2")');
    return evaluate("Fixture.startSignIn()");
  });
  assert.equal(seen.offered, true, seen.text);
  assert.match(seen.text, /Claude two's sign-in is still running\. Wait for it to finish, or open Claude two and cancel its sign-in, then try again\./, seen.text);
  assert.doesNotMatch(seen.text, /Clear stuck sign-in|didn't confirm/, seen.text);
  assert.deepEqual(seen.noted.filter((noted) => noted !== "ok"), ["native_auth_sign_in_running"], seen.text);
  assert.equal(STUCK_SIGN_IN_CODES.includes("native_auth_sign_in_running"), false, "a running sign-in raises no stuck banner");
});

test("a change refused because its account's sign-in still runs says so plainly, without Clear stuck sign-in", () => {
  const line = api.plainError("native_auth_sign_in_running");
  assert.equal(line, "A sign-in is still running. Wait for it to finish or cancel it, then try again.");
});
