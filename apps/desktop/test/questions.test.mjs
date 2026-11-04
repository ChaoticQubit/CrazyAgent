import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { QuestionRow } = await import("../src/screens/team/questions.tsx");

const forbidden = () => { throw new Error("static rendering must not open or answer"); };
const question = (id, asked_for, for_label) => ({
  id, agent: id, label: `Agent ${id}`, severity: "normal", asked_for, for_label,
  text: "May I delete the old fixtures?", assumption: null, created_ms: 1, terminal: null,
});
const row = (item) => renderToStaticMarkup(createElement(QuestionRow, { projectId: "project-a", question: item, onOpen: forbidden, onAnswered: forbidden }));

test("each question row says whom its asker meant it for (OP-50)", () => {
  assert.match(row(question(1, "me", "Me")), /For: Me</);
  assert.match(row(question(2, "master", "Master")), /For: Master</);
  const lead = row(question(3, "lead", "Ada (@lead-graph)"));
  assert.match(lead, /For: Ada \(@lead-graph\)</);
  assert.doesNotMatch(lead, /For: Me</);
});
