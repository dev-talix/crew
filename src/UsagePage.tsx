import { createMemo, createSignal, For, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { api, errorText, relativeTime, type Agent, type PlanLimits } from "./api";
import { RefreshIcon } from "./icons";
import { Page, Segmented } from "./ui";
import { claudeWindows, codexWindows, compact, ORIGINS, since, summarize, type LimitWindow, type Origin, type UsageRow } from "./usage";
import { BudgetBar, PlanGuardSetting } from "./BudgetControls";
import { familiarRows, type BudgetStatus } from "./budgets";
import "./usage.css";

const DAYS = 7;

function Meter(props: { window: LimitWindow }) {
  const high = () => props.window.percent >= 90;
  return (
    <div class="usage-meter">
      <div class="usage-meter-head">
        <span>{props.window.label}</span>
        <span class="usage-meter-value">
          {Math.round(props.window.percent)}% used
          <Show when={props.window.resetsAt}>{(at) => <span class="usage-faint">, resets {relativeTime(at())}</span>}</Show>
        </span>
      </div>
      <div
        class={["usage-track", { high: high() }]}
        role="meter"
        aria-label={props.window.label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(props.window.percent)}
      >
        <span style={{ width: `${Math.max(props.window.percent, 0.6)}%` }} />
      </div>
      <Show when={high()}>
        <p class="usage-warning">Almost at this limit.</p>
      </Show>
    </div>
  );
}

function PlanCard(props: { name: string; plan?: string | null; windows: LimitWindow[]; note?: string; empty: string }) {
  return (
    <div class="list usage-plan">
      <div class="usage-plan-head">
        <span class="list-title">{props.name}</span>
        <Show when={props.plan}>
          <span class="usage-faint">{props.plan}</span>
        </Show>
      </div>
      <Show when={props.windows.length > 0} fallback={<p class="settings-detail">{props.empty}</p>}>
        <For each={props.windows}>{(w) => <Meter window={w} />}</For>
      </Show>
      <Show when={props.note}>
        <p class="usage-faint usage-note">{props.note}</p>
      </Show>
    </div>
  );
}

const planName = (plan: unknown) => (typeof plan === "string" && plan ? `${plan.charAt(0).toUpperCase()}${plan.slice(1)} plan` : null);
const dayName = (start: number) => new Date(start).toLocaleDateString(undefined, { weekday: "short" });
const fullDay = (start: number) => new Date(start).toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric" });

export default function UsagePage(props: { agents: Agent[] }) {
  const [limits, setLimits] = createSignal<PlanLimits | null>(null);
  const [rows, setRows] = createSignal<UsageRow[] | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [view, setView] = createSignal<"source" | "familiar">("source");
  const [hovered, setHovered] = createSignal<number | null>(null);
  const [budgets, setBudgets] = createSignal<BudgetStatus[]>([]);

  const load = async () => {
    setLoading(true);
    setError("");
    const now = Date.now();
    const [plan, usage] = await Promise.allSettled([api.planLimits(), api.usageSince(since(now, DAYS))]);
    setBudgets(await api.listBudgets().catch(() => []));
    if (plan.status === "fulfilled") setLimits(plan.value);
    else setError(errorText(plan.reason));
    if (usage.status === "fulfilled") setRows(usage.value);
    else setError(errorText(usage.reason));
    setLoading(false);
  };
  onSettled(() => void load());

  const summary = createMemo(() => summarize(rows() ?? [], Date.now(), DAYS));
  const peak = () => Math.max(1, ...summary().days.map((d) => d.byOrigin.you + d.byOrigin.ritual + d.byOrigin.spell));
  const share = (origin: Origin) => (summary().total ? (summary().byOrigin[origin] / summary().total) * 100 : 0);
  const agentById = (id: string) => props.agents.find((a) => a.id === id);
  const top = () => summary().byAgent[0]?.tokens || 1;

  const refresh = (
    <button type="button" class={["ghost", "small", { spinning: loading() }]} disabled={loading()} onClick={() => void load()}>
      <RefreshIcon /> Refresh
    </button>
  );

  return (
    <Page title="Usage" actions={refresh}>
      <div class="settings usage">
        <Show when={error()}>
          <p class="error-box settings-error" role="alert">
            {error()}
          </p>
        </Show>

        <h2 class="section-title">Plan limits</h2>
        <div class="usage-plans">
          <PlanCard
            name="Codex"
            plan={planName((limits()?.codex as { rateLimits?: { planType?: string } } | null)?.rateLimits?.planType)}
            windows={codexWindows(limits()?.codex as Record<string, unknown> | null)}
            empty={limits() ? limits()?.codexError ?? "Codex didn't report any limits." : "Checking…"}
          />
          <PlanCard
            name="Claude"
            windows={claudeWindows((limits()?.claude?.info ?? null) as Record<string, unknown> | null)}
            note={limits()?.claude ? `As of ${relativeTime(limits()!.claude!.at)}, from the last Claude reply.` : undefined}
            empty="Claude reports its limits while it replies. They show here after a Claude familiar's next turn."
          />
        </div>

        <PlanGuardSetting />

        <h2 class="section-title">Grimoire, last {DAYS} days</h2>
        <div class="list usage-activity">
          <div class="usage-activity-head">
            <p class="usage-total">
              {compact(summary().total)} <span class="usage-faint">tokens</span>
            </p>
            <Show when={summary().total > 0}>
              <Segmented
                label="Group usage by"
                value={view()}
                options={[
                  { value: "source", label: "By source" },
                  { value: "familiar", label: "By familiar" },
                ]}
                onChange={setView}
              />
            </Show>
          </div>
          <Show
            when={summary().total > 0}
            fallback={<p class="settings-detail">No turns yet this week. Usage shows up here after your familiars reply.</p>}
          >
            <Show
              when={view() === "source"}
              fallback={
                <ul class="usage-familiars">
                  <For each={familiarRows(summary().byAgent, budgets())}>
                    {(entry) => {
                      const agent = () => agentById(entry.agentId);
                      return (
                        <li>
                          <span class="usage-familiar-name">
                            <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={18} alive={false} />}</Show>
                            {agent()?.name ?? "A deleted familiar"}
                          </span>
                          <span class="usage-familiar-bar">
                            <span style={{ width: `${(entry.tokens / top()) * 100}%`, background: agent()?.avatar.color ?? "var(--faint)" }} />
                          </span>
                          <span class="usage-familiar-value">{compact(entry.tokens)}</span>
                          <Show when={entry.budget}>{(b) => <BudgetBar status={b()} />}</Show>
                        </li>
                      );
                    }}
                  </For>
                </ul>
              }
            >
              <div class="usage-split" role="img" aria-label="Share of tokens by source">
                <For each={ORIGINS}>
                  {(o) => (
                    <Show when={summary().byOrigin[o.key] > 0}>
                      <span class={`usage-series-${o.key}`} style={{ "flex-grow": summary().byOrigin[o.key] }} />
                    </Show>
                  )}
                </For>
              </div>
              <ul class="usage-legend">
                <For each={ORIGINS}>
                  {(o) => (
                    <li>
                      <span class={`usage-swatch usage-series-${o.key}`} aria-hidden="true" />
                      <span>{o.label}</span>
                      <span class="usage-legend-value">
                        {compact(summary().byOrigin[o.key])} <span class="usage-faint">{Math.round(share(o.key))}%</span>
                      </span>
                    </li>
                  )}
                </For>
              </ul>

              <div class="usage-days" onMouseLeave={() => setHovered(null)}>
                <For each={summary().days}>
                  {(day, i) => {
                    const total = () => day.byOrigin.you + day.byOrigin.ritual + day.byOrigin.spell;
                    return (
                      <div
                        class={["usage-day", { hovered: hovered() === i() }]}
                        tabindex="0"
                        aria-label={`${fullDay(day.start)}: ${compact(total())} tokens`}
                        onMouseEnter={() => setHovered(i())}
                        onFocus={() => setHovered(i())}
                        onBlur={() => setHovered(null)}
                      >
                        <div class="usage-column" style={{ height: `${(total() / peak()) * 100}%` }}>
                          <For each={[...ORIGINS].reverse()}>
                            {(o) => (
                              <Show when={day.byOrigin[o.key] > 0}>
                                <span class={`usage-series-${o.key}`} style={{ "flex-grow": day.byOrigin[o.key] }} />
                              </Show>
                            )}
                          </For>
                        </div>
                        <span class="usage-day-label">{dayName(day.start)}</span>
                        <Show when={hovered() === i()}>
                          <div class={["usage-tip", { left: i() >= DAYS - 2 }]} role="tooltip">
                            <strong>{fullDay(day.start)}</strong>
                            <For each={ORIGINS}>
                              {(o) => (
                                <span>
                                  <span class={`usage-swatch usage-series-${o.key}`} aria-hidden="true" />
                                  {o.label} {compact(day.byOrigin[o.key])}
                                </span>
                              )}
                            </For>
                          </div>
                        </Show>
                      </div>
                    );
                  }}
                </For>
              </div>
            </Show>
          </Show>
        </div>
        <Show when={summary().total > 0}>
          <details class="usage-table">
            <summary>Show the numbers</summary>
            <table>
              <thead>
                <tr>
                  <th>Day</th>
                  <For each={ORIGINS}>{(o) => <th>{o.label}</th>}</For>
                </tr>
              </thead>
              <tbody>
                <For each={summary().days}>
                  {(day) => (
                    <tr>
                      <td>{fullDay(day.start)}</td>
                      <For each={ORIGINS}>{(o) => <td>{day.byOrigin[o.key].toLocaleString()}</td>}</For>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </details>
        </Show>
        <p class="hint settings-footnote">
          Grimoire counts the tokens each turn reports, input and output together. Plan limits come from your Codex and Claude accounts.
        </p>
      </div>
    </Page>
  );
}
