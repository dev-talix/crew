import { createSignal, For, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { api, errorText, onEvent, onMountAsync, relativeTime, type Agent, type ApprovalDecision, type InboxItem } from "./api";
import { inkFor } from "./avatar";
import { groupByFamiliar, isApproval, optionsInOrder } from "./inboxList";
import { EmptyState, Page } from "./ui";
import "./inbox.css";

/** The inbox list, kept fresh by the events that change it. */
function createInbox() {
  const [items, setItems] = createSignal<InboxItem[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const reload = () =>
    api.inbox().then(
      (list) => {
        setItems(list);
        setLoaded(true);
      },
      () => setLoaded(true),
    );
  onMountAsync(async (keep) => {
    for (const name of ["inbox_changed", "approval_requested", "approval_closed", "agents_changed"] as const) {
      keep(await onEvent(name, () => void reload()));
    }
    await reload();
  });
  return { items, loaded, reload, drop: (id: string) => setItems((list) => list.filter((i) => i.id !== id)) };
}

/** The sidebar link to the inbox, with how many decisions wait. */
export function InboxLink(props: { active: boolean; onClick: () => void }) {
  const { items } = createInbox();
  const count = () => items().length;
  return (
    <button
      class={["nav-item", { active: props.active }]}
      aria-current={props.active ? "page" : undefined}
      aria-label={count() ? `Inbox, ${count()} waiting` : "Inbox"}
      title="Inbox"
      onClick={props.onClick}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M2.5 9.5 4.2 3.4a.8.8 0 0 1 .8-.6h6a.8.8 0 0 1 .8.6l1.7 6.1V13a.5.5 0 0 1-.5.5H3a.5.5 0 0 1-.5-.5z" />
        <path d="M2.5 9.5h3l.8 1.6h3.4l.8-1.6h3" />
      </svg>
      <span>Inbox</span>
      <Show when={count()}>
        <span class="inbox-badge">{count()}</span>
      </Show>
    </button>
  );
}

/** Every familiar's open questions and pending approvals, answered with one tap. */
export default function InboxPage(props: { agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const inbox = createInbox();
  const [error, setError] = createSignal("");
  const agentById = (id: string) => props.agents.find((a) => a.id === id);

  const run = async (item: InboxItem, send: () => Promise<void>) => {
    setError("");
    inbox.drop(item.id);
    try {
      await send();
    } catch (e) {
      setError(errorText(e));
      void inbox.reload();
    }
  };
  const answer = (item: InboxItem, value: string) =>
    run(item, () => (isApproval(item) ? api.answerApproval(item.id, value as ApprovalDecision) : api.answerQuestion(item.id, value)));
  const dismiss = (item: InboxItem) => run(item, () => api.dismissQuestion(item.id));

  return (
    <Page title="Inbox">
      <div class="inbox">
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>
        <Show when={inbox.loaded() && inbox.items().length === 0}>
          <EmptyState
            title="Nothing to decide"
            body="When a familiar asks you something or needs your OK, it shows up here with the answer it recommends."
          />
        </Show>
        <For each={groupByFamiliar(inbox.items())}>
          {(group) => {
            const agent = () => agentById(group.agentId);
            const color = () => agent()?.avatar.color;
            return (
              <section
                class="inbox-group"
                aria-label={agent()?.name ?? "A familiar"}
                style={color() ? { "--agent": color()!, "--agent-ink": inkFor(color()!) } : undefined}
              >
                <button class="inbox-familiar" disabled={!agent()} onClick={() => props.onOpenAgent(group.agentId)}>
                  <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={22} alive={false} />}</Show>
                  <span>{agent()?.name ?? "A familiar"}</span>
                </button>
                <div class="list">
                  <For each={group.items}>{(item) => <InboxRow item={item} onAnswer={(v) => void answer(item, v)} onDismiss={() => void dismiss(item)} />}</For>
                </div>
              </section>
            );
          }}
        </For>
      </div>
    </Page>
  );
}

function InboxRow(props: { item: InboxItem; onAnswer: (value: string) => void; onDismiss: () => void }) {
  const [own, setOwn] = createSignal("");
  const approval = () => isApproval(props.item);
  const sendOwn = (e: SubmitEvent) => {
    e.preventDefault();
    if (own().trim()) props.onAnswer(own().trim());
  };
  return (
    <div class="inbox-item">
      <div class="inbox-text">
        <p class="inbox-title">{props.item.title}</p>
        <Show when={props.item.detail}>
          <Show when={props.item.kind === "command"} fallback={<p class="inbox-detail">{props.item.detail}</p>}>
            <pre class="inbox-command">{props.item.detail}</pre>
          </Show>
        </Show>
        <p class="inbox-meta">
          {approval() ? "Needs your OK. It counts as Deny after 2 minutes." : `Asked ${relativeTime(props.item.createdAt ?? Date.now())}`}
        </p>
      </div>
      <div class="inbox-options" role="group" aria-label="Answers">
        <For each={optionsInOrder(props.item)}>
          {(option) => {
            const recommended = () => option.value === props.item.recommended;
            return (
              <button class={recommended() ? "primary small inbox-recommended" : "ghost small"} onClick={() => props.onAnswer(option.value)}>
                {option.label}
                <Show when={recommended()}>
                  <span class="inbox-pick">Recommended</span>
                </Show>
              </button>
            );
          }}
        </For>
      </div>
      <Show when={!approval()}>
        <form class="inbox-own" onSubmit={sendOwn}>
          <input
            aria-label="Answer in your own words"
            placeholder="Or answer in your own words"
            maxlength={2000}
            value={own()}
            onInput={(e) => setOwn(e.currentTarget.value)}
          />
          <button type="submit" class="ghost small" disabled={!own().trim()}>
            Send
          </button>
          <button type="button" class="ghost small" onClick={props.onDismiss}>
            Dismiss
          </button>
        </form>
      </Show>
    </div>
  );
}
