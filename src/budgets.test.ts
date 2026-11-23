import { expect, test } from "bun:test";
import { budgetMeter, familiarRows, formatTokens, parseTokens, type BudgetStatus } from "./budgets";

test("reads token counts the way people type them", () => {
  expect(parseTokens("")).toBeNull();
  expect(parseTokens("  ")).toBeNull();
  expect(parseTokens("50000")).toBe(50_000);
  expect(parseTokens("50,000")).toBe(50_000);
  expect(parseTokens("50k")).toBe(50_000);
  expect(parseTokens("1.5M")).toBe(1_500_000);
  expect(parseTokens("2 b")).toBe(2_000_000_000);
  expect(parseTokens("0")).toBeNaN();
  expect(parseTokens("-5")).toBeNaN();
  expect(parseTokens("lots")).toBeNaN();
});

test("writes saved counts back short", () => {
  expect(formatTokens(50_000)).toBe("50k");
  expect(formatTokens(1_500_000)).toBe("1.5m");
  expect(formatTokens(1_100_000)).toBe("1.1m");
  expect(formatTokens(1234)).toBe("1234");
  expect(parseTokens(formatTokens(1_100_000))).toBe(1_100_000);
});

const status = (agentId: string, used: number, tokens = 50_000, period: "day" | "week" = "day"): BudgetStatus => ({ agentId, used, tokens, period, resetsAt: 0 });

test("draws a budget as a share of its window", () => {
  expect(budgetMeter(status("a", 12_000))).toEqual({ label: "12K of 50K today", percent: 24, over: false });
  expect(budgetMeter(status("a", 80_000, 50_000, "week"))).toEqual({ label: "80K of 50K this week", percent: 100, over: true });
});

test("lists familiars with usage, then budgeted ones without", () => {
  const rows = familiarRows([{ agentId: "a", tokens: 900 }, { agentId: "b", tokens: 10 }], [status("b", 10), status("c", 0)]);
  expect(rows.map((r) => [r.agentId, r.tokens, r.budget?.agentId ?? null])).toEqual([["a", 900, null], ["b", 10, "b"], ["c", 0, "c"]]);
});
