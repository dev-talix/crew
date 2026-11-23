import { expect, test } from "bun:test";
import { claudeWindows, codexWindows, compact, since, summarize, windowLabel, type UsageRow } from "./usage";

const now = new Date(2026, 10, 23, 15, 0).getTime();
const at = (daysAgo: number, hour = 12) => new Date(2026, 10, 23 - daysAgo, hour).getTime();
const row = (agentId: string, origin: string, tokens: number, when: number): UsageRow => ({ agentId, provider: "codex", origin, input: tokens - 10, output: 10, at: when });

test("summarizes the last seven days by source, familiar, and day", () => {
  const rows = [row("a1", "you", 100, at(0)), row("a2", "ritual", 300, at(1, 7)), row("a1", "spell", 50, at(6, 1)), row("a1", "you", 999, at(7))];
  const s = summarize(rows, now);
  expect(s.total).toBe(450);
  expect(s.byOrigin).toEqual({ you: 100, ritual: 300, spell: 50 });
  expect(s.byAgent).toEqual([{ agentId: "a2", tokens: 300 }, { agentId: "a1", tokens: 150 }]);
  expect(s.days).toHaveLength(7);
  expect(s.days[0].byOrigin.spell).toBe(50);
  expect(s.days[5].byOrigin.ritual).toBe(300);
  expect(s.days[6].byOrigin.you).toBe(100);
  expect(since(now, 7)).toBe(new Date(2026, 10, 17).getTime());
});

test("labels plan windows by their length", () => {
  expect(windowLabel(300)).toBe("5-hour limit");
  expect(windowLabel(10_080)).toBe("Weekly limit");
  expect(windowLabel(10_080, "Opus")).toBe("Weekly Opus limit");
  expect(windowLabel(null)).toBe("Usage limit");
  expect(compact(1234)).toBe("1.2K");
});

test("reads Codex and Claude limit replies", () => {
  const codex = codexWindows({ rateLimits: { primary: { usedPercent: 23, windowDurationMins: 300, resetsAt: 1_795_788_000 }, secondary: null } });
  expect(codex).toEqual([{ label: "5-hour limit", percent: 23, resetsAt: 1_795_788_000_000 }]);
  const claude = claudeWindows({ unifiedWindows: { seven_day: { utilization: 0.42, resetsAt: 1_795_788_000 }, seven_day_opus: { utilization: 7, resetsAt: null } } });
  expect(claude.map((w) => [w.label, Math.round(w.percent)])).toEqual([["Weekly limit", 42], ["Weekly Opus limit", 7]]);
  expect(claudeWindows({ rateLimitType: "five_hour", utilization: 0.1, resetsAt: 1 })[0].label).toBe("5-hour limit");
  expect(claudeWindows(null)).toEqual([]);
});
