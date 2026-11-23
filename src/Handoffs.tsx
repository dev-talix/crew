import { createSignal, For, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { api, onEvent, onMountAsync, relativeTime, type Agent, type Handoff, type Message, type SideDeltaEvent } from "./api";
import { linkLabel, startedReplying, STATUS_LABELS } from "./handoffList";
import SideConversation from "./SideConversation";
import { EmptyState, Page } from "./ui";
import "./handoffs.css";

/** Who handed what to whom, newest first, with where each one stands and its thread. */
export default function HandoffsPage(props: { agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const [handoffs, setHandoffs] = createSignal<Handoff[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [open, setOpen] = createSignal<string | null>(null);
  const reload = () =>
    api.listHandoffs().then(
      (list) => {
        setHandoffs(list);
        setLoaded(true);
      },
      () => setLoaded(true),
    );
  onMountAsync(async (keep) => {
    for (const name of ["side_message", "handoffs_changed", "agents_changed"] as const) {
      keep(await onEvent(name, () => void reload()));
    }
    keep(await onEvent<SideDeltaEvent>("side_delta", (e) => setHandoffs((list) => startedReplying(list, e.chatId))));
    await reload();
  });
  const agentById = (id: string) => props.agents.find((a) => a.id === id);

  return (
    <Page title="Handoffs">
      <div class="handoffs">
        <Show when={loaded() && handoffs().length === 0}>
          <EmptyState
            title="No handoffs yet"
            body="When a familiar asks or messages another familiar, it shows up here with who sent it, what it asked, and how it went."
          />
        </Show>
        <Show when={handoffs().length > 0}>
          <div class="list">
            <For each={handoffs()}>
              {(h) => {
                const from = () => agentById(h.fromId);
                const to = () => agentById(h.toId);
                const toName = () => to()?.name ?? "A familiar";
                const isOpen = () => open() === h.id;
                // The same row a chat shows for this conversation, so SideConversation opens it the same way.
                const link = (): Message => ({
                  id: 0,
                  agentId: h.fromId,
                  role: "side",
                  text: linkLabel(h, toName()),
                  status: "done",
                  source: h.id,
                  createdAt: h.createdAt,
                  updatedAt: null,
                });
                return (
                  <div class="handoff">
                    <div class="handoff-head">
                      <Familiar agent={from()} onOpen={props.onOpenAgent} />
                      <span class="handoff-arrow" aria-label={h.kind === "ask" ? "asked" : "messaged"}>
                        →
                      </span>
                      <Familiar agent={to()} onOpen={props.onOpenAgent} />
                      <span class="spacer" />
                      <span class={["handoff-status", h.status]}>{STATUS_LABELS[h.status]}</span>
                    </div>
                    <p class="handoff-what" title={h.what}>
                      {h.what}
                    </p>
                    <div class="handoff-meta">
                      <span>
                        {h.kind === "ask" ? "Asked" : "Messaged"} {relativeTime(h.createdAt)}
                      </span>
                      <button class="ghost small" aria-expanded={isOpen() ? "true" : "false"} onClick={() => setOpen(isOpen() ? null : h.id)}>
                        {isOpen() ? "Hide thread" : "Open thread"}
                      </button>
                    </div>
                    <Show when={isOpen()}>
                      <SideConversation message={link()} asker={from()?.name ?? "A familiar"} startOpen />
                    </Show>
                  </div>
                );
              }}
            </For>
          </div>
        </Show>
      </div>
    </Page>
  );
}

function Familiar(props: { agent: Agent | undefined; onOpen: (id: string) => void }) {
  return (
    <button class="handoff-familiar" disabled={!props.agent} onClick={() => props.agent && props.onOpen(props.agent.id)}>
      <Show when={props.agent}>{(a) => <BotAvatar avatar={a().avatar} size={20} alive={false} />}</Show>
      <span>{props.agent?.name ?? "A familiar"}</span>
    </button>
  );
}
