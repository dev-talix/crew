import { createEffect, createSignal, For, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync } from "./api";
import { LEDGER_STATUSES, ledgerIsBlank, ledgerUpdated, STATUS_NAMES, type Ledger as LedgerData, type LedgerStatus } from "./ledgers";
import "./ledger.css";

/** The open familiar's ledger under the chat bar: goal, status, and what it's waiting on, editable in place. */
export default function Ledger(props: { agentId: string; name: string }) {
  const [ledger, setLedger] = createSignal<LedgerData | null>(null);
  const [editing, setEditing] = createSignal(false);
  const [error, setError] = createSignal("");
  const [goal, setGoal] = createSignal("");
  const [waitingOn, setWaitingOn] = createSignal("");
  const [nextStep, setNextStep] = createSignal("");

  const load = (id: string) =>
    api.getLedger(id).then(
      (l) => id === props.agentId && setLedger(l),
      () => {},
    );
  createEffect(
    () => props.agentId,
    (id) => {
      setLedger(null);
      setEditing(false);
      setError("");
      void load(id);
    },
  );
  onMountAsync(async (keep) => keep(await onEvent("ledger_changed", () => void load(props.agentId))));

  const save = async (patch: Parameters<typeof api.saveLedger>[1]) => {
    const id = props.agentId;
    try {
      const saved = await api.saveLedger(id, patch);
      if (id !== props.agentId) return;
      setLedger(saved);
      setEditing(false);
      setError("");
    } catch (e) {
      setError(errorText(e));
      // Puts the status menu back to what's saved.
      void load(id);
    }
  };

  const startEditing = () => {
    const current = ledger();
    setGoal(current?.goal ?? "");
    setWaitingOn(current?.waitingOn ?? "");
    setNextStep(current?.nextStep ?? "");
    setError("");
    setEditing(true);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") setEditing(false);
  };

  return (
    <Show when={ledger()}>
      {(current) => (
        <div class={["ledger", { blank: ledgerIsBlank(current()) }]} data-slot="ledger">
          <Show
            when={editing()}
            fallback={
              <div class="ledger-row">
                <select
                  class={["ledger-status", current().status]}
                  aria-label="Status"
                  value={current().status}
                  onChange={(e) => void save({ status: e.currentTarget.value as LedgerStatus })}
                >
                  <For each={LEDGER_STATUSES}>{(s) => <option value={s}>{STATUS_NAMES[s]}</option>}</For>
                </select>
                <button class="ledger-summary" title="Edit the ledger" onClick={startEditing}>
                  <span class={["ledger-goal", { placeholder: !current().goal }]}>{current().goal || "No goal yet. Set one."}</span>
                  <Show when={current().waitingOn}>
                    <span class="ledger-waiting">Waiting on {current().waitingOn}</span>
                  </Show>
                </button>
                <span class="ledger-updated">{ledgerUpdated(current(), props.name)}</span>
              </div>
            }
          >
            <form
              class="ledger-form"
              onKeyDown={onKey}
              onSubmit={(e) => {
                e.preventDefault();
                void save({ goal: goal(), waitingOn: waitingOn(), nextStep: nextStep() });
              }}
            >
              <input aria-label="Goal" placeholder="Goal" maxlength={300} value={goal()} onInput={(e) => setGoal(e.currentTarget.value)} autofocus />
              <input
                aria-label="Waiting on"
                placeholder="Waiting on"
                maxlength={300}
                value={waitingOn()}
                onInput={(e) => setWaitingOn(e.currentTarget.value)}
              />
              <input
                aria-label="Next step"
                placeholder="Next step or notes"
                maxlength={1000}
                value={nextStep()}
                onInput={(e) => setNextStep(e.currentTarget.value)}
              />
              <div class="ledger-actions">
                <button type="button" class="ghost small" onClick={() => setEditing(false)}>
                  Cancel
                </button>
                <button type="submit" class="primary small">
                  Save
                </button>
              </div>
            </form>
          </Show>
          <Show when={!editing() && current().nextStep}>
            <p class="ledger-next">Next: {current().nextStep}</p>
          </Show>
          <Show when={error()}>
            <p class="ledger-error" role="alert">
              {error()}
            </p>
          </Show>
        </div>
      )}
    </Show>
  );
}
