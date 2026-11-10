import { createElement, type ReactElement } from "react";
import type { MoneyView, Tokens } from "../../api-usage";

export const COST_LABEL = "";

/** Writes nanodollars as dollars and cents. Input: nanodollars. Output: the text. */
export function dollarText(_nanos: number): string {
  return "";
}

/** Builds one figure's words. Inputs: the tokens, the dollars and the view. Output: the leading
 * number, the one beside it and the tooltip every dollar carries. */
export function figure(_tokens: Tokens, _costNanos: number, _money: MoneyView): { main: string; beside: string; title: string } {
  return { main: "", beside: "", title: "" };
}

/** Renders one figure. Inputs: the tokens, the dollars and the view. Output: the element. */
export function Figure(_props: { tokens: Tokens; costNanos: number; money: MoneyView }): ReactElement {
  return createElement("span");
}

/** Says what has no price. Input: the view. Output: the sentence, empty when all is priced. */
export function unpricedNote(_money: MoneyView): string {
  return "";
}
