import { click, reachAccountsStep, shot, shown, until } from "./settings-steps.mjs";

const theme = (label) => `//*[@role="radiogroup"][@aria-label="Theme"]//*[@role="radio"][normalize-space(.)="${label}"]`;
const readsDark = () => browser.execute(() => window.matchMedia("(prefers-color-scheme: dark)").matches);

/** Picks one Theme option in the Profile dialog and waits until the native window's own
 * appearance, which the webview reads as its colour scheme, follows. Inputs: the option label
 * and whether the window should read dark. Output: none; fails when the window never follows. */
async function choose(label, dark) {
  await click(theme(label));
  await until(async () => (await readsDark()) === dark, 10000, `the window does not read ${dark ? "dark" : "light"} after ${label}`);
}

describe("The native window follows the app theme on the real desktop app and engine (DEC-181)", () => {
  it("Dark and Light in Profile hold the window's appearance, and System gives it back to the Mac", async () => {
    await reachAccountsStep("Theme Owner");
    await click("button=Skip for now");
    await until(async () => !(await shown("#learn-offer-title")), 30000, "Home did not open after Skip for now");
    const system = await readsDark();
    await browser.execute(() => { window.location.hash = "#/settings/profile"; });
    await until(() => shown(theme("Dark")), 15000, "the Profile dialog did not show the Theme choice");
    await choose("Dark", true);
    await shot("app-theme-dark");
    await choose("Light", false);
    await shot("app-theme-light");
    await choose("System", system);
  });
});
