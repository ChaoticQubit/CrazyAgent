import assert from "node:assert/strict";
import { test } from "node:test";
import { SECRET_SOURCES, mask } from "./trial3-mask.mjs";

test("mask removes every kind of secret the evidence contract names", () => {
  const secrets = [
    "owner.name+trial@example.com",
    "3f2b9c4e-1a7d-4e8b-9c0d-5e6f7a8b9c0d",
    "Bearer abc.DEF-123_456~xyz=",
    "sk-ant-api03-AbCdEfGhIjKlMnOpQrSt",
    "ghp_AbCdEfGhIjKlMnOpQrStUvWx",
    "github_pat_11AbCdEfGhIjKlMnOpQrSt",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJl",
    "a".repeat(24) + "0123456789abcdef".repeat(2),
    "WDJB-MJHT",
    "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2RlZmdoaWpr",
  ];
  for (const secret of secrets) {
    const masked = mask(`before ${secret} after`);
    assert.equal(masked, "before [masked] after", `masks ${secret}`);
  }
});

test("mask keeps the text a trial step reports", () => {
  const kept = [
    "Land on reads trial3-land after reopening the Agents tab",
    "/Users/owner/Projects/ca-trial-3/private",
    "commit 0123456789abcdef0123456789abcdef01234567 on main",
    "Accepted as 3f2b9c4 on main.",
    "12848 bytes, first line # Mandatory workflow",
    "UTF-8 text, 2 change sets: Ready to review x2",
  ];
  for (const line of kept) assert.equal(mask(line), line, `keeps ${line}`);
});

test("the browser masks with the same patterns the node side uses", () => {
  const sample = "mail owner@example.com and key sk-AbCdEfGhIjKlMnOpQrSt";
  const browser = SECRET_SOURCES.map((source) => new RegExp(source, "g")).reduce((text, pattern) => text.replace(pattern, "[masked]"), sample);
  assert.equal(browser, mask(sample));
  assert.equal(browser, "mail [masked] and key [masked]");
});

test("the owner's screen text keeps its labels, while the same labels fused by textContent read as one masked token", () => {
  const fused = "ca-trial-3OverviewConversationAgentsChangesContextProcessSettings2 change sets are waiting";
  const lines = "ca-trial-3\nOverview\nConversation\nAgents\nChanges\nContext\nProcess\nSettings\n2 change sets are waiting";
  assert.equal(mask(fused), "[masked] change sets are waiting");
  assert.equal(mask(lines), lines);
});
