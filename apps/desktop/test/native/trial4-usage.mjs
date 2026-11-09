import assert from "node:assert/strict";

export const USE_IT_OR_LOSE_IT = "use_it_or_lose_it";
const WINDOWS = ["5-hour", "Weekly"];
const USED = /^\d{1,3}% used$/;
const RESETS = /^Resets in (?:under a minute|\d+ min|\d+ h \d+ min|\d+ d \d+ h)(?: \(your time\))?$/;

/** The sentence the Usage card shows for an API-key Codex account, which has no windows. */
export const API_KEY_READING = "An API-key account has no 5-hour or weekly window.";

/** Says whether Metrics shows every account's 5-hour and weekly use with a meter and
 * the time each window resets (OP-17); an API-key account's card saying it has no windows is
 * a valid reading, though not when every account is one. Input: the limit cards in screen
 * order, each { notice, windows: [{ label, used, meter, reset }] } as the page shows them.
 * Output: the detail; throws the first account and window that shows less, by its place so
 * no name is logged. */
export function limitsVerdict(cards) {
  assert(Array.isArray(cards), "the limit cards are a list");
  if (cards.length === 0) throw new Error("Metrics shows no account limits");
  const keyed = cards.filter((card) => card.notice === API_KEY_READING).length;
  if (keyed === cards.length) throw new Error(`not exercised: every one of the ${cards.length} accounts is an API-key account`);
  cards.forEach((card, at) => {
    if (card.notice === API_KEY_READING) return;
    const where = `account ${at + 1} of ${cards.length}`;
    const labels = card.windows.map((window) => window.label);
    if (labels.join(",") !== WINDOWS.join(",")) throw new Error(`${where} shows the windows ${JSON.stringify(labels)}, not 5-hour and Weekly`);
    for (const window of card.windows) {
      if (!USED.test(window.used)) throw new Error(`${where} ${window.label}: shows ${JSON.stringify(window.used)}, not how much is used`);
      if (!window.meter) throw new Error(`${where} ${window.label}: shows no use meter`);
      if (!RESETS.test(window.reset)) throw new Error(`${where} ${window.label}: shows ${JSON.stringify(window.reset)}, not when it resets`);
    }
  });
  const shown = `${cards.length - keyed} accounts each show their 5-hour and weekly use and reset time`;
  return keyed === 0 ? shown : `${shown}; ${keyed} API-key accounts have no 5-hour or weekly window`;
}

/** Says whether each pool that picks by Use it or lose it tries the account whose weekly
 * limit resets soonest first. Among the accounts a pool would try next, leaving out those
 * pinned to go last and those with no weekly room left (the engine ranks both after the
 * rest), a known reset comes before an unknown one and an earlier one before a later one.
 * Input: the engine's ranking, [{ pool, strategy, accounts: [{ connection, standing }] }].
 * Output: { pools: [{ pool, next }], detail } for the pools with two known resets to order;
 * throws the pool and places out of order, or why no pool could show the order. */
export function orderVerdict(rankings) {
  assert(Array.isArray(rankings), "the ranking is a list");
  const chosen = rankings.filter((ranking) => ranking.strategy === USE_IT_OR_LOSE_IT);
  if (chosen.length === 0) throw new Error(`not exercised: none of the ${rankings.length} pools picks accounts by Use it or lose it`);
  const pools = [];
  let known = 0;
  chosen.forEach((ranking, at) => {
    const next = ranking.accounts.filter(({ standing }) => standing.kind === "next");
    const ranked = next.filter(({ standing }) => !standing.use_last && standing.weekly_left_percent !== 0).map(({ standing }) => standing.weekly_resets_ms);
    for (let place = 1; place < ranked.length; place += 1) {
      const [before, now] = [ranked[place - 1], ranked[place]];
      if (now !== null && (before === null || now < before)) {
        throw new Error(`Use it or lose it pool ${at + 1} of ${chosen.length} tries the account at place ${place + 1}, whose weekly limit resets ${before === null ? "at a known time" : `${Math.round((before - now) / 60000)} min sooner`}, after the one at place ${place}`);
      }
    }
    const resets = ranked.filter((value) => value !== null).length;
    if (resets >= 2) {
      pools.push({ pool: ranking.pool, next: next.map(({ connection }) => connection) });
      known += resets;
    }
  });
  if (pools.length === 0) throw new Error(`not exercised: none of the ${chosen.length} Use it or lose it pools has two accounts to try next with a known weekly reset`);
  return { pools, detail: `${pools.length} of ${chosen.length} Use it or lose it pools try the soonest weekly reset first (${known} accounts with a known reset)` };
}

/** Says whether a pool's Next accounts list shows the accounts in the engine's order.
 * Inputs: the names in the engine's order and the names the list shows. Output: whether they
 * match. */
export function sameOrder(expected, shown) {
  assert(Array.isArray(expected) && Array.isArray(shown), "both orders are lists");
  return expected.length === shown.length && expected.every((name, at) => name === shown[at]);
}
