import { UI_MS, lit } from "../trial-kit.mjs";
import { limitsVerdict, orderVerdict, sameOrder } from "../trial4-usage.mjs";
import { selectPool } from "../settings-steps.mjs";

const PAGES_MAX = 5;

/** Calls one desktop command through the app's own Tauri invoke, as the page does. Inputs:
 * the command and its arguments. Output: the answer; throws the refusal. */
async function invoke(command, args = {}) {
  const answer = await browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }), (failure) => done({ failed: String(failure) }));
  }, command, args);
  if (answer?.failed !== undefined) throw new Error(`${command} was refused: ${answer.failed}`);
  return answer?.value;
}

/** Every account's name by its id, following the engine's pages. Output: a Map. */
async function accountNames() {
  const names = new Map();
  let after = null;
  for (let page = 0; page < PAGES_MAX; page += 1) {
    const read = await invoke("desktop_list_accounts", { after, limit: 200 });
    for (const account of read.accounts) names.set(account.id, account.name);
    if (!read.next) return names;
    after = read.next;
  }
  throw new Error(`there are more than ${PAGES_MAX * 200} accounts to name`);
}

/** The limit cards Metrics shows, in screen order. Output: [{ notice, windows }]. */
function limitCards() {
  return browser.execute(() => [...document.querySelectorAll("section.usage-limits")].map((card) => ({
    notice: card.querySelector(".notice")?.innerText.trim() ?? "",
    windows: [...card.querySelectorAll(".usage-window")].map((window) => ({
      label: window.querySelector(".usage-window-head strong")?.innerText.trim() ?? "",
      used: window.querySelector(".usage-window-head span")?.innerText.trim() ?? "",
      meter: window.querySelector("meter.usage-meter") !== null,
      reset: window.querySelector(":scope > span.muted")?.innerText.trim() ?? "",
    })),
  })));
}

/** The account names one pool's Next accounts list shows in order. Input: the section's
 * label. Output: the names, or null while the section is missing. */
function shownNext(label) {
  return browser.execute((label) => {
    const section = [...document.querySelectorAll("section")].find((node) => node.getAttribute("aria-label") === label);
    return section ? [...section.querySelectorAll("ol > li > span.grow")].map((node) => node.innerText.trim()) : null;
  }, label);
}

export const checks = [{
  id: "N-D16",
  op: ["OP-17"],
  title: "Metrics shows each account's 5-hour and weekly use and reset, and Use it or lose it tries the soonest weekly reset first",
  why: "owner: where are the usage limits ... there is no usage pane in the settings",
  after: "end",
  run: async (kit) => {
    await kit.go("#/metrics");
    await kit.until(() => browser.execute(() => document.querySelector("section.usage-limits") !== null || document.body.innerText.includes("No limits read yet")),
      UI_MS, "Metrics never showed the account limits");
    const limits = limitsVerdict(await limitCards());
    await kit.inView("section.usage-limits");
    await kit.screen();
    const pools = new Map((await invoke("desktop_pool_summaries")).map((pool) => [pool.id, pool.name]));
    const names = await accountNames();
    let order = orderVerdict(await invoke("desktop_account_ranking"));
    let pool = order.pools[0];
    await kit.waitFor(async () => {
      try {
        order = orderVerdict(await invoke("desktop_account_ranking"));
      } catch (failure) {
        return { done: false, seen: String(failure?.message ?? failure) };
      }
      pool = order.pools[0];
      const expected = pool.next.map((connection) => names.get(connection) ?? connection);
      const shown = await shownNext(`${pools.get(pool.pool)} next accounts`);
      return { done: shown !== null && sameOrder(expected, shown), seen: `engine order of ${expected.length} accounts, Next accounts list shows ${shown === null ? "no section" : `${shown.length} rows in ${sameOrder(expected, shown) ? "the same" : "another"} order`}` };
    }, UI_MS, "the Next accounts list never showed the engine's order");
    const name = pools.get(pool.pool);
    await kit.inView(`//section[@aria-label=${lit(`${name} next accounts`)}]`);
    await kit.screen();
    await kit.go("#/settings/pools");
    await selectPool(name);
    const picked = () => browser.execute(() => document.querySelector('[role="radiogroup"][aria-label="Account order"] [aria-checked="true"]')?.textContent ?? null);
    await kit.until(async () => (await picked()) !== null, UI_MS, "Settings, Pools never showed the pool's account order");
    const chosen = await picked();
    if (!chosen.includes("Use it or lose it")) throw new Error(`the pool's account order reads ${JSON.stringify(chosen)}, not Use it or lose it`);
    await kit.inView(`//*[@role="radiogroup" and @aria-label=${lit("Account order")}]`);
    await kit.screen();
    return `${limits}; ${order.detail}; its Next accounts list shows that order and its account order reads ${chosen}`;
  },
}];
