import { createSignal, For, Show } from "solid-js";
import { api, onEvent, onMountAsync, relativeTime, type Agent, type Message } from "./api";
import { checkInNote, MIN_EVERY, type CheckInRun, type CheckInSettings, type CheckInStatus } from "./checkins";
import SideConversation from "./SideConversation";
import { Toggle } from "./ui";
import "./helpers.css";
import "./routines.css";
import "./checkins.css";

const DAY_NAMES = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const ALL_DAYS = [1, 2, 3, 4, 5, 6, 7];

const STATUS: Record<CheckInRun["status"], string> = {
  running: "Running",
  done: "Done",
  failed: "Failed",
  timeout: "Timed out",
  stopped: "Stopped",
  interrupted: "Interrupted",
};

/** The row SideConversation expects, pointing at the check-in's hidden chat. */
const transcriptRow = (run: CheckInRun): Message => ({
  id: 0,
  agentId: run.agentId,
  role: "side",
  text: "Check-in",
  status: "done",
  source: run.chatId,
  createdAt: run.startedAt,
  updatedAt: null,
});

/**
 * The familiar settings group for check-ins: the schedule, the memory review switch, and the recent
 * check-ins that left something for the person, each with its transcript.
 */
export function CheckInsField(props: { agent: Agent; value: CheckInSettings; status: CheckInStatus | null; onChange: (next: CheckInSettings) => void }) {
  const set = (patch: Partial<CheckInSettings>) => props.onChange({ ...props.value, ...patch });
  // Shown days. An empty saved list means every day, so it shows all seven.
  const shown = () => (props.value.days.length ? props.value.days : ALL_DAYS);
  const toggleDay = (day: number) => {
    const next = shown().includes(day) ? shown().filter((d) => d !== day) : [...shown(), day].sort();
    if (next.length) set({ days: next.length === 7 ? [] : next });
  };
  const [runs, setRuns] = createSignal<CheckInRun[]>([]);
  const load = () => {
    const id = props.agent.id;
    api.listCheckins(id).then((list) => id === props.agent.id && setRuns(list), () => undefined);
  };
  onMountAsync(async (keep) => {
    load();
    keep(await onEvent("checkins_changed", load));
  });

  return (
    <div class="field wide checkins">
      <span>Check-ins</span>
      <div class="list">
        <div class="list-row">
          <div class="list-main">
            <span class="list-title">Check in on its own</span>
            <span class="list-sub">It looks over its ledger, questions, rituals, and drafts, and only bothers you when something needs you.</span>
          </div>
          <Toggle label="Check in on its own" checked={props.value.enabled} onChange={(enabled) => set({ enabled })} />
        </div>
        <Show when={props.value.enabled}>
          <div class="list-row checkin-schedule">
            <label class="checkin-inline">
              Every
              <input
                type="number"
                aria-label="Minutes between check-ins"
                min={MIN_EVERY}
                max={1440}
                step={5}
                value={props.value.everyMinutes}
                onInput={(e) => set({ everyMinutes: Number(e.currentTarget.value) })}
              />
              minutes, from
              <input type="time" aria-label="Check-ins start" value={props.value.start} onInput={(e) => set({ start: e.currentTarget.value })} />
              to
              <input type="time" aria-label="Check-ins end" value={props.value.end} onInput={(e) => set({ end: e.currentTarget.value })} />
            </label>
            <div class="routine-days" role="group" aria-label="Check-in days">
              <For each={ALL_DAYS}>
                {(day) => (
                  <button
                    type="button"
                    class="routine-day"
                    aria-label={DAY_NAMES[day - 1]}
                    aria-pressed={shown().includes(day) ? "true" : "false"}
                    onClick={() => toggleDay(day)}
                  >
                    {DAY_NAMES[day - 1][0]}
                  </button>
                )}
              </For>
            </div>
          </div>
        </Show>
        <div class="list-row">
          <div class="list-main">
            <span class="list-title">Review each chat for things to remember</span>
            <span class="list-sub">When you start a new chat, it looks back over the last one and saves what's worth keeping to its memory.</span>
          </div>
          <Toggle label="Review each chat for things to remember" checked={props.value.reviewChats} onChange={(reviewChats) => set({ reviewChats })} />
        </div>
      </div>
      <p class="hint">
        Check-ins run with workspace-only access and can only ask you, draft, or update their ledger and memory. A check-in that finds
        nothing leaves no trace.
        <Show when={props.status && checkInNote(props.status)}>{(note) => <> {note()}</>}</Show>
      </p>
      <Show when={runs().length > 0}>
        <div class="checkin-runs" aria-label="Recent check-ins">
          <For each={runs()} keyed={(r: CheckInRun) => r.id}>
            {(run) => (
              <div class="checkin-run">
                <div class="checkin-meta">
                  <span class={["helper-status", run().status]}>{STATUS[run().status]}</span>
                  <span class="checkin-time">{relativeTime(run().startedAt)}</span>
                </div>
                <Show when={run().summary}>{(summary) => <p class="checkin-summary">{summary()}</p>}</Show>
                <SideConversation message={transcriptRow(run())} asker={props.agent.name} />
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
