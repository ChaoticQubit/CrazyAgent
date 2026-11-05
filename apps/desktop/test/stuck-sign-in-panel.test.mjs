import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

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
