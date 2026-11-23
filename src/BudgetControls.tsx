import { createSignal, onSettled, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, relativeTime } from "./api";
import { budgetMeter, type BudgetPeriod, type BudgetStatus, type PlanGuard } from "./budgets";
import { Segmented, Toggle } from "./ui";
import "./budgets.css";

/** The familiar settings field: a token count, empty for no limit, per day or per week. */
export function BudgetField(props: { text: string; period: BudgetPeriod; onText: (text: string) => void; onPeriod: (period: BudgetPeriod) => void }) {
  return (
    <div class="field wide">
      <span>Token budget</span>
      <div class="budget-field">
        <input
          aria-label="Token budget"
          inputmode="numeric"
          value={props.text}
          placeholder="No limit"
          onInput={(e) => props.onText(e.currentTarget.value)}
        />
        <Segmented
          label="Budget resets"
          value={props.period}
          options={[
            { value: "day", label: "Per day" },
            { value: "week", label: "Per week" },
          ]}
          onChange={props.onPeriod}
        />
      </div>
      <p class="hint">Like 50000 or 50k. Once it's used up, new turns wait for the next {props.period === "day" ? "day" : "week"}, rituals skip, and spells stop at this familiar.</p>
    </div>
  );
}

/** A familiar's budget on the Usage page. */
export function BudgetBar(props: { status: BudgetStatus }) {
  const meter = () => budgetMeter(props.status);
  return (
    <span class="budget-bar">
      <span
        class={["usage-track", { high: meter().percent >= 90 && !meter().over, over: meter().over }]}
        role="meter"
        aria-label="Budget used"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(meter().percent)}
      >
        <span style={{ width: `${Math.max(meter().percent, 0.6)}%` }} />
      </span>
      <span class={["budget-label", { over: meter().over }]}>
        {meter().over ? `Over budget, ${meter().label}` : `Budget ${meter().label}`}
        <span class="usage-faint">, resets {relativeTime(props.status.resetsAt)}</span>
      </span>
    </span>
  );
}

/** A short notice on Home and Rituals while the plan limit guard holds rituals back. */
export function RitualPauseNotice() {
  const [reason, setReason] = createSignal<string | null>(null);
  const load = () => api.ritualPause().then(setReason, () => setReason(null));
  onMountAsync(async (keep) => {
    void load();
    keep(await onEvent("routines_changed", () => void load()));
  });
  return (
    <Show when={reason()}>
      <p class="ritual-pause" role="status">
        <strong>Rituals are paused.</strong> {reason()}
      </p>
    </Show>
  );
}

/** The Usage page setting: pause rituals when a plan limit passes a percentage. */
export function PlanGuardSetting() {
  const [guard, setGuard] = createSignal<PlanGuard>({ enabled: true, percent: 90 });
  const [percent, setPercent] = createSignal("90");
  const [error, setError] = createSignal("");
  onSettled(() => {
    api.getPlanGuard().then(
      (g) => {
        setGuard(g);
        setPercent(String(g.percent));
      },
      (e) => setError(errorText(e)),
    );
  });

  const save = async (next: PlanGuard) => {
    const previous = guard();
    setGuard(next);
    setError("");
    try {
      setGuard(await api.savePlanGuard(next));
    } catch (e) {
      setGuard(previous);
      setPercent(String(previous.percent));
      setError(errorText(e));
    }
  };

  const commitPercent = () => {
    const n = Number(percent());
    if (Number.isInteger(n) && n >= 1 && n <= 100) {
      if (n !== guard().percent) void save({ ...guard(), percent: n });
    } else {
      setPercent(String(guard().percent));
      setError("Pick a percentage from 1 to 100.");
    }
  };

  return (
    <>
      <div class="list plan-guard-list">
        <div class="list-row">
          <div class="list-main">
            <label class="list-title plan-guard" for="plan-guard-percent">
              Pause rituals when a plan limit passes
              <input
                id="plan-guard-percent"
                type="number"
                min="1"
                max="100"
                value={percent()}
                disabled={!guard().enabled}
                onInput={(e) => setPercent(e.currentTarget.value)}
                onChange={commitPercent}
              />
              %
            </label>
            <p class="settings-detail">Rituals skip and say why in their list. Your own messages and spells still run.</p>
          </div>
          <Toggle label="Pause rituals near a plan limit" checked={guard().enabled} onChange={(enabled) => void save({ ...guard(), enabled })} />
        </div>
      </div>
      <Show when={error()}>
        <p class="error-box settings-error" role="alert">
          {error()}
        </p>
      </Show>
    </>
  );
}
