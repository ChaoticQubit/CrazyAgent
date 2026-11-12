import { randomBytes } from "node:crypto";
import { click, shown, texts } from "../settings-steps.mjs";
import { UI_MS, lit } from "../trial-kit.mjs";
import { memoryVerdict, rememberPrompt } from "../trial4-context.mjs";
import { sendToMaster, terminalLive } from "./n-d8-master-reply.mjs";

const MASTER_MS = 600000;
const MEMORY = "//button[contains(@class, \"pj-titem\")][normalize-space(.)=\"Project memory\"]";
const WRITES = "#memory-agent-writes";
const SETTINGS_LINK = "//button[normalize-space(.)=\"Memory settings\"]";

/** Opens Context, Project memory afresh, so it reads what the engine saved. Input: the kit.
 * Output: the text the panel shows. */
async function projectMemory(kit) {
  await kit.reopen("context");
  await click(MEMORY);
  await kit.until(() => shown("//h2[normalize-space(.)=\"Project memory\"]"), UI_MS, "Context, Project memory did not open");
  return (await texts("main")).join("\n");
}

/** Reads the source line Settings, Memory shows under one memory row, through WebDriver since
 * the row is found by XPath. Input: the row's XPath. Output: its text, "" when none is shown. */
async function sourceOf(row) {
  const line = `${row}//span[contains(@class, "set-help")]`;
  return (await shown(line)) ? $(line).getText() : "";
}

export const checks = [{
  id: "N-D24",
  op: ["OP-22"],
  title: "The master remembers a fact, Context, Project memory shows it and approves it in place",
  why: "owner: \"the memory doesn't work at all\"",
  after: "changes",
  run: async (kit) => {
    const token = `T4-${randomBytes(4).toString("hex")}`;
    await projectMemory(kit);
    await click(SETTINGS_LINK);
    await kit.until(() => shown(WRITES), UI_MS, "Memory settings in Context, Project memory never opened Agent memory writes");
    const writes = await $(WRITES).getValue();
    await kit.tab("conversation");
    const request = rememberPrompt(token);
    await terminalLive(kit, MASTER_MS);
    await sendToMaster(kit, request);
    await kit.waitFor(async () => {
      const saved = (await projectMemory(kit)).includes(token);
      return { done: saved, seen: saved ? `${token} is listed` : `${token} is not listed yet` };
    }, MASTER_MS, `OP-22: Context, Project memory never showed the master's fact ${token}`);

    const row = `//div[contains(@class, "set-row")][.//p[contains(normalize-space(.), ${lit(token)})]]`;
    await kit.go("#/settings/memory");
    await kit.pick("#memory-scope", kit.project());
    await kit.until(() => shown(row), UI_MS, `Settings, Memory never listed ${token} for the project`);
    const source = await sourceOf(row);

    const note = `//li[contains(@class, "pj-memory")][.//p[contains(normalize-space(.), ${lit(token)})]]`;
    const approve = `${note}//button[normalize-space(.)="Approve"]`;
    const waiting = await projectMemory(kit);
    const approved = await shown(approve);
    if (approved) {
      await click(approve);
      await kit.until(async () => !(await shown(approve)) && (await $(note).getText()).includes("Saved"), UI_MS,
        `${token} still waits after Approve in Context, Project memory`);
    }
    const listed = await projectMemory(kit);
    await kit.screen();
    return memoryVerdict({ token, writes, source, approved, waiting, shown: listed });
  },
}];
