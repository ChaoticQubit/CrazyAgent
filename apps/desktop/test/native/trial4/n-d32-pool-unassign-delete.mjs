const SCRATCH_URL = "http://127.0.0.1:9/v1";
const LOOP_TEXT = "assigned to a project";

/** Calls one of the app's own commands, as the app's screens do. Inputs: the command and its
 * arguments. Output: the answer, or { failed } with the plain error. */
function invoke(command, args) {
  return browser.executeAsync((name, given, done) => {
    window.__TAURI_INTERNALS__.invoke(name, given).then(done, (failure) => done({ failed: String(failure) }));
  }, command, args);
}

/** Presses the first element under the selector whose text is exactly the given text.
 * Inputs: the selector and the text. Output: whether one was pressed. */
function press(selector, text) {
  return browser.execute((query, wanted) => {
    const found = [...document.querySelectorAll(query)].find((node) => node.textContent.trim() === wanted && !node.disabled);
    found?.click();
    return Boolean(found);
  }, selector, text);
}

/** Waits until an element with the exact text can be pressed, then presses it. Inputs: the
 * kit, the selector, the text and the failure label. */
async function pressWhenShown(kit, selector, text, label) {
  await kit.until(() => press(selector, text), kit.UI_MS, label);
}

/** Opens a row's More menu and picks one item. Inputs: the kit, the menu label and the item. */
async function menuPick(kit, menu, item) {
  await kit.until(() => browser.execute((label) => {
    const button = [...document.querySelectorAll("button[aria-label]")].find((node) => node.getAttribute("aria-label") === label);
    if (button && button.getAttribute("aria-expanded") !== "true") button.click();
    return Boolean(button);
  }, menu), kit.UI_MS, `no ${menu} menu`);
  await pressWhenShown(kit, '[role="menu"] [role="menuitem"]', item, `the ${menu} menu has no ${item}`);
}

/** Reads the open confirm dialog: its title, body and button labels. Output: the dialog or null. */
function dialog() {
  return browser.execute(() => {
    const open = document.querySelector("dialog[open]");
    if (!open) return null;
    return { text: open.innerText, buttons: [...open.querySelectorAll("button")].map((button) => button.textContent.trim()) };
  });
}

/** Confirms the open dialog with its danger button after checking it names what changes.
 * Inputs: the kit, what the body must say and the confirm label. Output: the body text. */
async function confirm(kit, says, label) {
  const seen = JSON.parse(await kit.waitFor(async () => {
    const open = await dialog();
    return { done: open !== null, seen: JSON.stringify(open) };
  }, kit.UI_MS, "no confirm dialog opened"));
  if (!seen.text.includes(says)) throw new Error(`the confirm does not say "${says}": ${seen.text}`);
  if (!seen.buttons.includes(label)) throw new Error(`the confirm offers ${seen.buttons.join(", ")}, not ${label}`);
  await kit.screen();
  await pressWhenShown(kit, "dialog[open] button", label, `${label} could not be pressed`);
  await kit.until(async () => (await dialog()) === null, kit.UI_MS, "the confirm dialog stayed open");
  return seen.text;
}

/** The text the window shows, read without a screenshot: the page, an open dialog and toasts.
 * Output: the text. */
function text() {
  return browser.execute(() => [...document.querySelectorAll("main, dialog[open], .toast")].map((node) => node.innerText).join("\n"));
}

/** Waits for the window to say a line, and fails on the old loop back to an assigned pool.
 * Inputs: the kit, the line and the failure label. */
async function says(kit, line, label) {
  const text = await kit.waitFor(async () => {
    const shown = await text();
    return { done: shown.includes(line) || shown.includes(LOOP_TEXT), seen: shown.slice(0, 600) };
  }, kit.UI_MS, label);
  if (!text.includes(line) && text.includes(LOOP_TEXT)) throw new Error(`looped back to "${LOOP_TEXT}": ${text}`);
}

/** Picks one Pool choice on the project's Settings and saves it. Inputs: the kit, the radio
 * value (empty for No pool) and the line the save shows. */
async function choosePool(kit, value, line) {
  await kit.go(`#/projects/${encodeURIComponent(kit.project())}/settings?section=pool`);
  await kit.until(() => browser.execute((wanted) => {
    const radio = [...document.querySelectorAll('input[type="radio"][name="ps-pool"]')].find((input) => input.value === wanted);
    radio?.click();
    return Boolean(radio?.checked);
  }, value), kit.UI_MS, `no Pool choice ${value || "No pool"}`);
  await pressWhenShown(kit, "main button.btn.primary", "Save pool", "Save pool could not be pressed");
  await says(kit, line, `the project did not say ${line}`);
}

export const checks = [{
  id: "N-D32",
  op: ["OP-56"],
  title: "A pool assigned to a project can be set to No pool and deleted, and an account that was in it deleted",
  why: "owner: a pool assigned to a project cannot be unassigned or deleted; an account in a pool cannot be deleted",
  after: "end",
  run: async (kit) => {
    const name = `ca-trial-scratch-${Date.now()}`;
    const added = await invoke("desktop_add_account", { draft: { provider: "local", method: "url", name, url: SCRATCH_URL, model: "scratch" }, key: null });
    const account = added?.value ?? added;
    if (!account?.id) throw new Error(`the scratch account was not added: ${JSON.stringify(added).slice(0, 300)}`);
    const original = { value: null };
    const lines = [];
    try {
      await kit.go("#/settings/pools?action=new");
      await (await $("#pool-name")).setValue(name);
      await (await $('input[type="search"][placeholder="Search accounts"]')).setValue(name);
      await kit.until(() => browser.execute((wanted) => {
        const row = [...document.querySelectorAll("label.pick")].find((node) => node.textContent.includes(wanted));
        const box = row?.querySelector('input[type="checkbox"]');
        if (box && !box.checked) box.click();
        return Boolean(box?.checked);
      }, name), kit.UI_MS, "the scratch account could not be put in the pool");
      await pressWhenShown(kit, 'main button[type="submit"]', "Save pool", "Save pool could not be pressed");
      await kit.until(() => browser.execute((wanted) => [...document.querySelectorAll(".pool-row .name")].some((node) => node.textContent.trim() === wanted), name),
        kit.UI_MS, "the scratch pool is not listed");
      await kit.go(`#/projects/${encodeURIComponent(kit.project())}/settings?section=pool`);
      const choices = JSON.parse(await kit.waitFor(async () => {
        const seen = await browser.execute((wanted) => {
          const radios = [...document.querySelectorAll('input[type="radio"][name="ps-pool"]')];
          const mine = radios.find((input) => input.closest("label")?.textContent.includes(wanted));
          return { checked: radios.find((input) => input.checked)?.value ?? null, pool: mine?.value ?? null };
        }, name);
        return { done: seen.pool !== null, seen: JSON.stringify(seen) };
      }, kit.UI_MS, "the project's Pool choices do not list the scratch pool"));
      original.value = choices.checked;
      await choosePool(kit, choices.pool, `${name} is assigned.`);
      lines.push("scratch pool assigned");
      await choosePool(kit, "", "This project has no pool now.");
      lines.push("set to No pool");
      await kit.go("#/settings/pools");
      await menuPick(kit, `More for ${name}`, "Delete pool");
      await confirm(kit, "Only the pool goes away.", "Delete pool");
      await says(kit, `${name} deleted.`, "the pool delete was not confirmed");
      lines.push("pool deleted after a confirm that names what stays");
      await kit.go("#/settings/accounts");
      await (await $('input[type="search"][placeholder="Search accounts"]')).setValue(name);
      await menuPick(kit, `More for ${name}`, "Delete");
      await confirm(kit, name, "Delete account");
      await kit.until(() => browser.execute((label) => ![...document.querySelectorAll("button[aria-label]")].some((node) => node.getAttribute("aria-label") === label), `More for ${name}`),
        kit.UI_MS, "the scratch account is still listed");
      lines.push("account deleted after its confirm");
      return lines.join("; ");
    } finally {
      if (original.value !== null && original.value !== "") await choosePool(kit, original.value, "is assigned.");
    }
  },
}];
