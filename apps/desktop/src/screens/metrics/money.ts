import { createElement, type ReactElement } from "react";
import type { MoneyView, Tokens } from "../../api-usage";
import { compactCount, headlineTokens } from "../settings/usage";

export const COST_LABEL = "Estimated cost at API prices";
const NANOS = 1_000_000_000;
const CENT_NANOS = NANOS / 100;

/** Writes nanodollars as dollars and cents. Input: nanodollars. Output: the text. */
export function dollarText(nanos: number): string {
  if (nanos > 0 && nanos < CENT_NANOS / 2) return "<$0.01";
  return (nanos / NANOS).toLocaleString("en-US", { style: "currency", currency: "USD" });
}

/** Builds one figure's words. Inputs: the tokens, the dollars and the view. Output: the leading
 * number, the one beside it and the tooltip every dollar carries. */
export function figure(tokens: Tokens, costNanos: number, money: MoneyView): { main: string; beside: string; title: string } {
  const count = compactCount(headlineTokens(tokens));
  if (money.settings.prices === "off") return { main: count, beside: "", title: "" };
  const cost = dollarText(costNanos);
  const title = `${COST_LABEL}, prices as of ${money.as_of}`;
  return money.shown === "dollars" ? { main: cost, beside: `${count} tokens`, title } : { main: count, beside: cost, title };
}

/** Renders one figure. Inputs: the tokens, the dollars and the view. Output: the element. */
export function Figure({ tokens, costNanos, money }: { tokens: Tokens; costNanos: number; money: MoneyView }): ReactElement {
  const shown = figure(tokens, costNanos, money);
  return createElement("span", { className: "mx-figure", title: shown.title }, createElement("b", null, shown.main), shown.beside && createElement("small", null, ` ${shown.beside}`));
}

/** Says what has no price. Input: the view. Output: the sentence, empty when all is priced. */
export function unpricedNote(money: MoneyView): string {
  return money.unpriced_tokens > 0 ? `${compactCount(money.unpriced_tokens)} tokens have no price, so they are left out of the dollars.` : "";
}
