// Token budgets per familiar: types, the token field's parser, and what the Usage page draws.
import { compact } from "./usage";

export type BudgetPeriod = "day" | "week";

export interface Budget {
  tokens: number;
  period: BudgetPeriod;
}

/** A familiar's budget and what this day or week has used. */
export interface BudgetStatus extends Budget {
  agentId: string;
  used: number;
  resetsAt: number;
}

/** Pause rituals when a plan limit passes `percent`. */
export interface PlanGuard {
  enabled: boolean;
  percent: number;
}

export interface RitualSkip {
  routineId: string;
  reason: string;
  at: number;
}

const UNITS: Record<string, number> = { "": 1, k: 1_000, m: 1_000_000, b: 1_000_000_000 };

/**
 * Reads a token count like "50000", "50,000", "50k", or "1.5M". Empty means no budget (null);
 * anything else that isn't a positive count is NaN.
 */
export function parseTokens(text: string): number | null {
  const clean = text.trim().toLowerCase().replace(/[,_\s]/g, "");
  if (!clean) return null;
  const match = /^(\d+(?:\.\d+)?)([kmb]?)$/.exec(clean);
  if (!match) return NaN;
  const tokens = Math.round(Number(match[1]) * UNITS[match[2]]);
  return tokens >= 1 ? tokens : NaN;
}

/** How a saved count shows in the field: 50000 as "50k", 1500000 as "1.5m", 1234 as "1234". */
export function formatTokens(tokens: number): string {
  for (const [unit, size] of [["b", 1e9], ["m", 1e6], ["k", 1e3]] as const) {
    if (tokens >= size && tokens % (size / 10) === 0) return `${tokens / size}${unit}`;
  }
  return String(tokens);
}

const WINDOW: Record<BudgetPeriod, string> = { day: "today", week: "this week" };

/** "12K of 50K today", with how full the bar is (0 to 100) and whether it's used up. */
export function budgetMeter(status: BudgetStatus): { label: string; percent: number; over: boolean } {
  return {
    label: `${compact(status.used)} of ${compact(status.tokens)} ${WINDOW[status.period]}`,
    percent: Math.min(100, (status.used / Math.max(status.tokens, 1)) * 100),
    over: status.used >= status.tokens,
  };
}

/** The Usage page's familiar rows: everyone with tokens this week, then anyone else with a budget. */
export function familiarRows(byAgent: { agentId: string; tokens: number }[], budgets: BudgetStatus[]) {
  const rows = byAgent.map((entry) => ({ ...entry, budget: budgets.find((b) => b.agentId === entry.agentId) ?? null }));
  for (const budget of budgets) {
    if (!rows.some((r) => r.agentId === budget.agentId)) rows.push({ agentId: budget.agentId, tokens: 0, budget });
  }
  return rows;
}
