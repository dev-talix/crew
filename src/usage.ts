// Turns Grimoire's per-turn usage rows into what the Usage page draws.

export type Origin = "you" | "ritual" | "spell";

export interface UsageRow {
  agentId: string;
  provider: string;
  origin: Origin | string;
  input: number;
  output: number;
  at: number;
}

export const ORIGINS: { key: Origin; label: string }[] = [
  { key: "you", label: "You" },
  { key: "ritual", label: "Rituals" },
  { key: "spell", label: "Spells" },
];

export interface Day {
  start: number;
  byOrigin: Record<Origin, number>;
}

export interface Summary {
  total: number;
  byOrigin: Record<Origin, number>;
  /** Familiars by tokens, most first. */
  byAgent: { agentId: string; tokens: number }[];
  /** One entry per day, oldest first, ending today. */
  days: Day[];
}

const DAY = 86_400_000;
const empty = (): Record<Origin, number> => ({ you: 0, ritual: 0, spell: 0 });
const originOf = (origin: string): Origin => (origin === "ritual" || origin === "spell" ? origin : "you");

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** The local midnight `days - 1` days before today, where the summary starts. */
export function since(now: number, days: number): number {
  return startOfDay(now) - (days - 1) * DAY;
}

export function summarize(rows: UsageRow[], now: number, days = 7): Summary {
  const first = since(now, days);
  const dayStarts = Array.from({ length: days }, (_, i) => startOfDay(first + i * DAY + DAY / 2));
  const daily = dayStarts.map((start) => ({ start, byOrigin: empty() }));
  const byOrigin = empty();
  const byAgent = new Map<string, number>();
  let total = 0;
  for (const row of rows) {
    if (row.at < first) continue;
    const tokens = row.input + row.output;
    const origin = originOf(row.origin);
    total += tokens;
    byOrigin[origin] += tokens;
    byAgent.set(row.agentId, (byAgent.get(row.agentId) ?? 0) + tokens);
    const day = [...daily].reverse().find((d) => d.start <= row.at);
    if (day) day.byOrigin[origin] += tokens;
  }
  return {
    total,
    byOrigin,
    byAgent: [...byAgent].map(([agentId, tokens]) => ({ agentId, tokens })).sort((a, b) => b.tokens - a.tokens),
    days: daily,
  };
}

/** 1234 -> "1.2K", 3400000 -> "3.4M". */
export function compact(n: number): string {
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(n);
}

/** Names a plan limit window by its length: 300 minutes is the 5-hour limit, a week is weekly. */
export function windowLabel(minutes: number | null | undefined, model?: string): string {
  const what = model ? ` ${model}` : "";
  if (!minutes) return `Usage${what} limit`;
  if (minutes === 10_080) return `Weekly${what} limit`;
  if (minutes % 1440 === 0) return `${minutes / 1440}-day${what} limit`;
  if (minutes % 60 === 0) return `${minutes / 60}-hour${what} limit`;
  return `${minutes}-minute${what} limit`;
}

export interface LimitWindow {
  label: string;
  /** 0 to 100. */
  percent: number;
  /** Milliseconds since the epoch. */
  resetsAt: number | null;
}

// Both CLIs report reset times in seconds; accept milliseconds too.
const toMs = (t: unknown) => (typeof t === "number" ? (t < 1e12 ? t * 1000 : t) : null);
const clamp = (n: number) => Math.max(0, Math.min(100, n));

type Json = Record<string, unknown> | null | undefined;

/** The windows in Codex's `account/rateLimits/read` reply. */
export function codexWindows(limits: Json): LimitWindow[] {
  const snapshot = (limits?.rateLimits ?? null) as Json;
  return (["primary", "secondary"] as const).flatMap((key) => {
    const w = snapshot?.[key] as Json;
    if (!w || typeof w.usedPercent !== "number") return [];
    return [{ label: windowLabel(w.windowDurationMins as number | null), percent: clamp(w.usedPercent), resetsAt: toMs(w.resetsAt) }];
  });
}

const CLAUDE_WINDOWS: Record<string, [number, string?]> = {
  five_hour: [300],
  seven_day: [10_080],
  seven_day_opus: [10_080, "Opus"],
  seven_day_sonnet: [10_080, "Sonnet"],
};

/** The windows in Claude's `rate_limit_event` info. Utilization comes as a fraction. */
export function claudeWindows(info: Json): LimitWindow[] {
  const windows = (info?.unifiedWindows ?? null) as Record<string, Json> | null;
  const entries: [string, Json][] = windows
    ? Object.entries(windows)
    : info && typeof info.rateLimitType === "string"
      ? [[info.rateLimitType, info]]
      : [];
  return entries.flatMap(([key, w]) => {
    if (!w || typeof w.utilization !== "number") return [];
    const [minutes, model] = CLAUDE_WINDOWS[key] ?? [0, key.replace(/_/g, " ")];
    const percent = w.utilization <= 1 ? w.utilization * 100 : w.utilization;
    return [{ label: windowLabel(minutes, model), percent: clamp(percent), resetsAt: toMs(w.resetsAt) }];
  });
}
