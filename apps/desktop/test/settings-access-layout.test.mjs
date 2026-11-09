import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { CONTRAST_MIN, press, tabRunner, tabWalk } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-access-layout.mjs", import.meta.url).pathname);
const run = tabRunner(fixture);
const onAccounts = (scene, check, ...rest) => run({ tab: "accounts", ...scene }, check, ...rest);
const onGitHub = (scene, check, ...rest) => run({ tab: "github", ...scene }, check, ...rest);
const onProfile = (scene, check, ...rest) => run({ tab: "profile", ...scene }, check, ...rest);
const DIALOG = "dialog[open]";
const top = (page) => page.evaluate("Fixture.top()");
const read = (page) => page.evaluate("Fixture.sections()");
const say = (page) => page.evaluate("Fixture.text()");
const sent = (page) => page.evaluate("Fixture.commands()");
const alert = (page) => page.evaluate("Fixture.alert()");
const count = (page, selector) => page.evaluate(`Fixture.count(${JSON.stringify(selector)})`);
const wordsOf = (page, selector) => page.evaluate(`Fixture.wordsOf(${JSON.stringify(selector)})`);
const dialog = (page) => wordsOf(page, DIALOG);
const open = (page) => count(page, DIALOG);
const button = (page, words) => press(page, "button", words);
const inDialog = (page, words) => press(page, `${DIALOG} button`, words);

/** Types with real input into a field. Inputs: the page, the selector and the words. */
async function typed(page, selector, words) {
  await press(page, selector);
  await page.send("Input.insertText", { text: words });
  await page.evaluate("Fixture.settle(80)");
}

test("Accounts' top shows its title, purpose and Add account", async () => {
  const seen = await onAccounts({}, top);
  assert.equal(seen?.heading, "Accounts", JSON.stringify(seen));
  assert.equal(seen.purpose, "ChatGPT, Claude, API key and local model accounts your agents can use.");
  assert.deepEqual(seen.actions, ["Add account"]);
});

test("Your accounts holds the search, the filters, the count and every account", async () => {
  const seen = await onAccounts({}, async (page) => ({
    sections: await read(page), search: await count(page, ".set-group .set-bar input[type=search]"), selects: await count(page, ".set-group .set-bar select"),
    tally: await wordsOf(page, ".set-group .set-bar .count"), head: await wordsOf(page, ".set-group .list-head"), rows: await count(page, ".set-group .account-row"), old: await count(page, ".toolbar"),
  }));
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Your accounts"]);
  assert.equal(seen.search, 1);
  assert.equal(seen.selects, 2);
  assert.equal(seen.tally, "3 accounts");
  assert.equal(seen.head, "NameProviderSign-in methodStatusPools");
  assert.equal(seen.rows, 3);
  assert.equal(seen.old, 0);
});

for (const width of [1180, 760, 700]) {
  test(`at ${width} px the list stays in its column and every row's menu is in view`, async () => {
    const seen = await onAccounts({}, (page) => page.evaluate(`(() => {
      const main = document.querySelector("#main");
      const column = document.querySelector(".settings-content").getBoundingClientRect();
      const add = document.querySelector(".settings-top button").getBoundingClientRect();
      const group = document.querySelector(".set-group").getBoundingClientRect();
      const list = document.querySelector(".windowed").getBoundingClientRect();
      const menu = document.querySelector('button[aria-label="More for Account 0"]').getBoundingClientRect();
      return { sideways: main.scrollWidth - main.clientWidth, columnRight: column.right, addRight: add.right, groupRight: group.right, table: document.querySelector(".account-table").getBoundingClientRect().right, listRight: list.right, menuRight: menu.right };
    })()`), width);
    assert.equal(seen.sideways, 0, JSON.stringify(seen));
    assert.ok(seen.addRight <= seen.columnRight + 0.5, JSON.stringify(seen));
    assert.ok(seen.groupRight <= seen.columnRight + 0.5, JSON.stringify(seen));
    assert.ok(seen.listRight <= seen.groupRight + 0.5, JSON.stringify(seen));
    assert.ok(seen.menuRight <= seen.listRight + 0.5, JSON.stringify(seen));
  });
}

test("at 1180 px, the size the app opens at, the list needs no sideways scroll", async () => {
  const seen = await onAccounts({}, (page) => page.evaluate(`(() => {
    const table = document.querySelector(".account-table");
    const list = document.querySelector(".windowed");
    return { table: table.scrollWidth - table.clientWidth, list: list.scrollWidth - list.clientWidth };
  })()`), 1180);
  assert.deepEqual(seen, { table: 0, list: 0 });
});

test("the longest status word stays whole inside its column at 1180 px", async () => {
  const seen = await onAccounts({ accounts: "long_status" }, (page) => page.evaluate(`(() => {
    const badge = document.querySelector(".account-row .badge");
    const cell = badge.parentElement.getBoundingClientRect();
    const row = document.querySelector(".account-row").getBoundingClientRect();
    const box = badge.getBoundingClientRect();
    return { clipped: badge.scrollWidth - badge.clientWidth, right: box.right, cellRight: cell.right, bottom: box.bottom, rowBottom: row.bottom };
  })()`), 1180);
  assert.ok(seen.clipped <= 0, JSON.stringify(seen));
  assert.ok(seen.right <= seen.cellRight + 0.5, JSON.stringify(seen));
  assert.ok(seen.bottom <= seen.rowBottom + 0.5, JSON.stringify(seen));
});

test("at 700 px the rows scroll sideways with the head and the menu stays in view over them", async () => {
  const seen = await onAccounts({}, (page) => page.evaluate(`(() => {
    const list = document.querySelector(".windowed");
    const overflow = list.scrollWidth - list.clientWidth;
    list.scrollLeft = 60;
    const head = list.querySelector(".list-head span");
    const name = list.querySelector(".account-row .cell");
    const menu = list.querySelector('button[aria-label="More for Account 0"]');
    return { overflow, scrolled: list.scrollLeft, headLeft: head?.getBoundingClientRect().left, nameLeft: name?.getBoundingClientRect().left,
      listRight: list.getBoundingClientRect().right, menuRight: menu.getBoundingClientRect().right, filled: getComputedStyle(menu.closest(".menu")).backgroundColor };
  })()`), 700);
  assert.ok(seen.overflow > 0, JSON.stringify(seen));
  assert.equal(seen.scrolled, 60, JSON.stringify(seen));
  assert.ok(Math.abs(seen.headLeft - seen.nameLeft) <= 1, JSON.stringify(seen));
  assert.ok(seen.menuRight <= seen.listRight + 0.5, JSON.stringify(seen));
  assert.notEqual(seen.filled, "rgba(0, 0, 0, 0)", JSON.stringify(seen));
});

test("the head stays in view while a long list scrolls down", async () => {
  const seen = await onAccounts({ accounts: "many" }, (page) => page.evaluate(`(async () => {
    const list = document.querySelector(".windowed");
    list.scrollTop = 400;
    await Fixture.settle(40);
    return { scrolled: list.scrollTop, listTop: list.getBoundingClientRect().top, headTop: document.querySelector(".list-head").getBoundingClientRect().top };
  })()`), 1180);
  assert.equal(seen.scrolled, 400, JSON.stringify(seen));
  assert.ok(seen.headTop <= seen.listTop + 1, JSON.stringify(seen));
});

test("a row given focus is not left under the head", async () => {
  const seen = await onAccounts({ accounts: "many" }, (page) => page.evaluate(`(async () => {
    const list = document.querySelector(".windowed");
    const head = document.querySelector(".list-head");
    list.scrollTop = 482;
    await Fixture.settle(40);
    const row = [...list.querySelectorAll(".account-row")].find((item) => item.textContent.includes("Account 10"));
    row.focus();
    await Fixture.settle(40);
    return { rowTop: row.getBoundingClientRect().top, headBottom: head.getBoundingClientRect().bottom };
  })()`), 1180);
  assert.ok(seen.rowTop >= seen.headBottom - 0.5, JSON.stringify(seen));
});

test("a row's open menu shows over the rows below it", async () => {
  await onAccounts({}, async (page) => {
    await press(page, 'button[aria-label="More for Account 0"]');
    await page.evaluate("Fixture.settle(40)");
    const seen = await page.evaluate(`(() => {
      const items = [...document.querySelectorAll('[role="menuitem"]')];
      const last = items.at(-1)?.getBoundingClientRect();
      if (!last) return { items: 0, hit: false };
      return { items: items.length, hit: document.elementFromPoint(last.x + last.width / 2, last.y + last.height / 2)?.closest('[role="menuitem"]') !== null };
    })()`);
    assert.ok(seen.items > 0, JSON.stringify(seen));
    assert.equal(seen.hit, true, JSON.stringify(seen));
  });
});

test("Add account opens its dialog, also straight from the add route", async () => {
  await onAccounts({}, async (page) => {
    await button(page, "Add account");
    assert.match(await dialog(page), /Add an account/);
  });
  assert.match(await onAccounts({ action: "add" }, dialog), /Add an account/);
});

test("an account row opens its drawer", async () => {
  await onAccounts({}, async (page) => {
    await press(page, ".account-row");
    assert.equal(await count(page, "dialog.drawer[open]"), 1);
    assert.match(await wordsOf(page, "dialog.drawer[open] h2"), /Account 0/);
  });
});

test("a row's Delete asks first, and Cancel keeps the account", async () => {
  await onAccounts({}, async (page) => {
    await press(page, 'button[aria-label="More for Account 0"]');
    await press(page, "[role=menuitem]", "Delete");
    assert.match(await dialog(page), /Delete Account 0\?/);
    await inDialog(page, "Cancel");
    assert.equal(await open(page), 0);
    assert.ok(!(await sent(page)).includes("desktop_remove_account"));
  });
});

test("a Delete the engine refuses says so below the top line", async () => {
  await onAccounts({}, async (page) => {
    await press(page, 'button[aria-label="More for Account 0"]');
    await press(page, "[role=menuitem]", "Delete");
    await inDialog(page, "Delete account");
    const seen = await alert(page);
    assert.match(seen.words, /Account 0/);
    assert.equal(seen.belowTop, true);
  });
});

test("ticking an account shows the selection bar inside the section, and Clear selection hides it", async () => {
  await onAccounts({}, async (page) => {
    await press(page, ".account-pick");
    assert.equal(await wordsOf(page, '.set-group [role=group][aria-label="Selected accounts"] .count'), "1 selected");
    const words = await wordsOf(page, '.set-group [role=group][aria-label="Selected accounts"]');
    for (const part of ["New pool from selected", "Delete selected", "Clear selection"]) assert.ok(words.includes(part), `${part} in ${words}`);
    await button(page, "Clear selection");
    assert.equal(await count(page, '[aria-label="Selected accounts"]'), 0);
  });
});

test("a search with no match says so and Clear filters brings the list back", async () => {
  await onAccounts({}, async (page) => {
    await typed(page, ".set-bar input[type=search]", "zzz");
    assert.match(await say(page), /No accounts match\./);
    await button(page, "Clear filters");
    assert.equal(await count(page, ".account-row"), 3);
  });
});

test("with no accounts the empty state keeps its Add account and the top has none", async () => {
  await onAccounts({ accounts: "empty" }, async (page) => {
    assert.deepEqual((await top(page)).actions, []);
    assert.match(await say(page), /No accounts yet/);
    await button(page, "Add account");
    assert.match(await dialog(page), /Add an account/);
  });
});

for (const [state, words] of [["error", /Couldn't load accounts\./], ["missing", /Accounts isn't ready yet/]]) {
  test(`Accounts' ${state} read keeps the title and says why, with no Add account above it`, async () => {
    const seen = await onAccounts({ accounts: state }, async (page) => ({ top: await top(page), words: await say(page), sections: await read(page) }));
    assert.equal(seen.top?.heading, "Accounts");
    assert.deepEqual(seen.top.actions, []);
    assert.match(seen.words, words);
    assert.deepEqual(seen.sections, []);
  });
}

test("Accounts' read still running keeps the title and draws no section", async () => {
  const seen = await onAccounts({ accounts: "pending" }, async (page) => ({ top: await top(page), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Accounts");
  assert.deepEqual(seen.sections, []);
});

test("real Tab keys go from Add account to the search, the filters and the first tick box", async () => {
  const walked = await onAccounts({}, (page) => tabWalk(page, 5, "id"));
  const names = await onAccounts({}, (page) => tabWalk(page, 5));
  assert.equal(names[0], "Add account");
  assert.deepEqual(walked.slice(2, 4), ["account-provider", "account-status"]);
  assert.equal(names[4], "Select Account 0");
});

test("GitHub's top shows its title and purpose with no action", async () => {
  const seen = await onGitHub({}, top);
  assert.equal(seen?.heading, "GitHub", JSON.stringify(seen));
  assert.equal(seen.purpose, "GitHub lets you clone your repositories into the projects folder. Agents commit locally; you push.");
  assert.deepEqual(seen.actions, []);
});

test("signed in, GitHub account has one line: who and how left, Sign out right", async () => {
  const [section] = await onGitHub({}, read);
  assert.equal(section.heading, "GitHub account");
  const [line] = section.rows;
  assert.equal(section.rows.length, 1);
  assert.equal(line.label, "Signed in to GitHub as mira.");
  assert.match(line.help, /keeps this sign-in in your Mac's keychain/);
  assert.deepEqual(line.buttons, ["Sign out of GitHub"]);
  assert.ok(line.controlLeft > line.labelRight, JSON.stringify(line));
});

test("signed in through the GitHub CLI, the help says what Sign out leaves", async () => {
  const [section] = await onGitHub({ github: "cli" }, read);
  assert.match(section.rows[0].help, /Signing out here leaves the GitHub CLI signed in\./);
});

test("a problem with the sign-in shows as an alert in the section", async () => {
  await onGitHub({ github: "trouble" }, async (page) => {
    assert.match((await alert(page)).words, /The sign-in expired\./);
    assert.equal(await count(page, '.set-group [role="alert"]'), 1);
  });
});

test("Sign out asks first, and Cancel sends nothing", async () => {
  await onGitHub({}, async (page) => {
    await button(page, "Sign out of GitHub");
    assert.match(await dialog(page), /Sign out of GitHub\?/);
    await inDialog(page, "Cancel");
    assert.equal(await open(page), 0);
    assert.ok(!(await sent(page)).includes("desktop_github_sign_out"));
  });
});

for (const [state, words] of [["error", /./], ["in", /GitHub is still signed in\. Try again\./]]) {
  test(`a Sign out that ends ${state === "in" ? "still signed in" : "in a failure"} says so in the section`, async () => {
    await onGitHub({ signOut: state }, async (page) => {
      await button(page, "Sign out of GitHub");
      await inDialog(page, "Sign out");
      assert.equal(await open(page), 0);
      assert.match(await wordsOf(page, '.set-group [role="alert"]'), words);
    });
  });
}

test("signed out, the top stays over the sign-in choices, which are unchanged", async () => {
  await onGitHub({ github: "out" }, async (page) => {
    assert.equal((await top(page)).heading, "GitHub");
    assert.match(await wordsOf(page, "section.in-panel h2"), /Sign in to GitHub/);
    assert.deepEqual(await read(page), []);
  });
});

for (const [state, words] of [["error", /./], ["missing", /Signing in to GitHub isn't ready yet/]]) {
  test(`GitHub's ${state} read keeps the title and says why`, async () => {
    const seen = await onGitHub({ github: state }, async (page) => ({ top: await top(page), words: await say(page) }));
    assert.equal(seen.top?.heading, "GitHub");
    assert.match(seen.words, words);
  });
}

test("GitHub's read still running keeps the title", async () => {
  assert.equal((await onGitHub({ github: "pending" }, top))?.heading, "GitHub");
});

test("Profile's top shows its title and purpose with no action", async () => {
  const seen = await onProfile({}, top);
  assert.equal(seen?.heading, "Profile", JSON.stringify(seen));
  assert.equal(seen.purpose, "Agents read this before they work for you.");
  assert.deepEqual(seen.actions, []);
});

test("Profile then Appearance, each field on its own line", async () => {
  const sections = await onProfile({}, read);
  assert.deepEqual(sections.map((section) => section.heading), ["Profile", "Appearance"]);
  assert.deepEqual(sections[0].rows.map((row) => row.label), ["Name", "Role", "Short bio", "Preferred stack", "Replies", "Before acting"]);
  assert.deepEqual(sections[1].rows.map((row) => row.label), ["Theme", "Master input"]);
  const by = Object.fromEntries(sections.flatMap((section) => section.rows).map((row) => [row.label, row]));
  for (const label of ["Name", "Role", "Replies", "Before acting", "Theme", "Master input"]) assert.ok(by[label].controlLeft > by[label].labelRight, `${label} ${JSON.stringify(by[label])}`);
  for (const label of ["Short bio", "Preferred stack"]) assert.ok(by[label].controlLeft < by[label].labelRight, `${label} stacks ${JSON.stringify(by[label])}`);
  assert.equal(by.Theme.help, "System follows your Mac. This choice is kept in this app only.");
  assert.match(by["Master input"].help, /Where you type to a live master/);
});

test("each field has its one label, the wide fields fill their line, and the helpers stay linked", async () => {
  const seen = await onProfile({}, (page) => page.evaluate(`(() => {
    const labels = (id) => [...document.getElementById(id).labels].map((label) => label.textContent.replace(/changed$/, ""));
    const slack = (id) => document.getElementById(id).closest(".set-row").getBoundingClientRect().width - document.getElementById(id).getBoundingClientRect().width;
    return { name: labels("profile-name"), role: labels("profile-role"), bio: labels("profile-bio"), stack: labels("profile-stack"), bioSlack: slack("profile-bio"), roleHelp: document.getElementById("profile-role").getAttribute("aria-describedby") };
  })()`));
  assert.deepEqual(seen.name, ["Name"]);
  assert.deepEqual(seen.role, ["Role"]);
  assert.deepEqual(seen.bio, ["Short bio"]);
  assert.deepEqual(seen.stack, ["Preferred stack"]);
  assert.equal(seen.bioSlack, 32, JSON.stringify(seen));
  assert.equal(seen.roleHelp, "profile-role-helper");
});

test("the chosen stack lines up with its field", async () => {
  const seen = await onProfile({}, (page) => page.evaluate(`(() => {
    const chip = document.querySelector(".set-row .chips .chip").getBoundingClientRect();
    const field = document.getElementById("profile-stack").getBoundingClientRect();
    return { chipLeft: chip.left, fieldLeft: field.left };
  })()`));
  assert.ok(Math.abs(seen.chipLeft - seen.fieldLeft) <= 1, JSON.stringify(seen));
});

test("Save changes waits for a change and then sends the profile", async () => {
  await onProfile({}, async (page) => {
    assert.equal(await page.evaluate('document.querySelector("form[aria-label=Profile] button[type=submit]").disabled'), true);
    await typed(page, "#profile-role", "x");
    await button(page, "Save changes");
    assert.ok((await sent(page)).includes("desktop_save_owner_profile"));
  });
});

test("a blank name is refused at its field and nothing is sent", async () => {
  await onProfile({ profile: "blank" }, async (page) => {
    await typed(page, "#profile-role", "x");
    await button(page, "Save changes");
    assert.equal(await page.evaluate('Fixture.invalid("#profile-name")'), "true");
    assert.ok(!(await sent(page)).includes("desktop_save_owner_profile"));
  });
});

test("a save the engine refuses says so inside the section", async () => {
  await onProfile({ save: "error" }, async (page) => {
    await typed(page, "#profile-role", "x");
    await button(page, "Save changes");
    assert.match((await alert(page)).words, /Couldn't save your profile\./);
    assert.equal(await count(page, '.set-group [role="alert"]'), 1);
  });
});

test("Theme changes the app's look from its line", async () => {
  await onProfile({}, async (page) => {
    await press(page, '[role=radiogroup][aria-label="Theme"] button', "Dark");
    assert.equal(await page.evaluate("document.documentElement.dataset.theme"), "dark");
  });
});

for (const [state, words] of [["error", /Couldn't load your profile\./], ["not_ready", /Your profile isn't ready yet/]]) {
  test(`Profile's ${state} read keeps the title and Appearance and says why`, async () => {
    const seen = await onProfile({ profile: state }, async (page) => ({ top: await top(page), words: await say(page), sections: await read(page) }));
    assert.equal(seen.top?.heading, "Profile");
    assert.match(seen.words, words);
    assert.deepEqual(seen.sections.map((section) => section.heading), ["Appearance"]);
  });
}

test("Profile still loading keeps the title and Appearance, with no form", async () => {
  const seen = await onProfile({ profile: "loading" }, async (page) => ({ top: await top(page), form: await count(page, "form[aria-label=Profile]"), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Profile");
  assert.equal(seen.form, 0);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Appearance"]);
});

test("real Tab keys walk the profile's fields in order", async () => {
  const walked = await onProfile({}, (page) => tabWalk(page, 3, "id"));
  assert.deepEqual(walked, ["profile-name", "profile-role", "profile-bio"]);
});

for (const theme of ["light", "dark"]) {
  test(`purpose, help and count words read at 4.5 to 1 in ${theme}`, async () => {
    const accounts = await onAccounts({}, async (page) => ({ purpose: await page.evaluate('Fixture.contrast(".settings-top p")'), count: await page.evaluate('Fixture.contrast(".set-bar .count")') }), 1180, theme);
    const github = await onGitHub({}, async (page) => ({ help: await page.evaluate('Fixture.contrast(".set-help")') }), 1180, theme);
    const profile = await onProfile({}, async (page) => ({ help: await page.evaluate('Fixture.contrast(".set-help")'), field: await page.evaluate('Fixture.contrast(".set-row .helper")') }), 1180, theme);
    for (const [name, ratio] of Object.entries({ ...accounts, ...github, ...profile })) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
