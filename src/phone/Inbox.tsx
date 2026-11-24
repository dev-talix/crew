import { createSignal, For, Show } from "solid-js";
import { isApproval } from "../inboxList";
import { api, errorText, type Familiar, type InboxItem } from "./api";
import { FamiliarFace } from "./Face";
import { phoneOptions, sortInbox, timeAgo } from "./logic";

export function InboxList(props: {
  items: InboxItem[];
  familiars: Familiar[];
  setItems: (fn: (items: InboxItem[]) => InboxItem[]) => void;
  reload: () => Promise<void>;
}) {
  const [error, setError] = createSignal("");
  const familiar = (id: string) => props.familiars.find((f) => f.id === id);

  const run = async (item: InboxItem, send: () => Promise<unknown>) => {
    setError("");
    props.setItems((list) => list.filter((i) => i.id !== item.id));
    try {
      await send();
    } catch (e) {
      setError(errorText(e));
      await props.reload();
    }
  };

  return (
    <section class="page" aria-label="Inbox">
      <h1 class="page-title">Inbox</h1>
      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>
      <Show when={props.items.length === 0}>
        <div class="empty">
          <p class="empty-title">Nothing to decide</p>
          <p>When a familiar asks you something or needs your OK, it shows up here.</p>
        </div>
      </Show>
      <div class="cards">
        <For each={sortInbox(props.items)}>
          {(item) => (
            <InboxCard
              item={item}
              familiar={familiar(item.agentId)}
              onAnswer={(answer) => void run(item, () => api.answer(item, answer))}
              onDismiss={() => void run(item, () => api.dismiss(item))}
            />
          )}
        </For>
      </div>
    </section>
  );
}

function InboxCard(props: { item: InboxItem; familiar?: Familiar; onAnswer: (answer: string) => void; onDismiss: () => void }) {
  const [own, setOwn] = createSignal("");
  const approval = () => isApproval(props.item);
  const sendOwn = (e: SubmitEvent) => {
    e.preventDefault();
    if (own().trim()) props.onAnswer(own().trim());
  };
  return (
    <article class={["card", { urgent: approval() }]} style={props.familiar ? { "--agent": props.familiar.avatar.color } : undefined}>
      <div class="card-who">
        <FamiliarFace familiar={props.familiar} size={24} />
        <span>{props.familiar?.name ?? "A familiar"}</span>
        <span class="card-when">{approval() ? "Needs your OK" : props.item.createdAt ? timeAgo(props.item.createdAt) : ""}</span>
      </div>
      <h2 class="card-title">{props.item.title}</h2>
      <Show when={props.item.detail}>
        <Show when={props.item.kind === "command"} fallback={<p class="card-detail">{props.item.detail}</p>}>
          <pre class="command">{props.item.detail}</pre>
        </Show>
      </Show>
      <div class="options" role="group" aria-label="Answers">
        <For each={phoneOptions(props.item)}>
          {(option) => {
            const recommended = () => !approval() && option.value === props.item.recommended;
            const tone = () => (approval() ? (option.value === "deny" ? "ghost" : "primary") : recommended() ? "primary" : "ghost");
            return (
              <button class={tone()} onClick={() => props.onAnswer(option.value)}>
                {option.label}
                <Show when={recommended()}>
                  <span class="pick">Recommended</span>
                </Show>
              </button>
            );
          }}
        </For>
      </div>
      <Show
        when={!approval()}
        fallback={<p class="card-note">Counts as Deny after 2 minutes. Always allow is on your Mac.</p>}
      >
        <form class="own" onSubmit={sendOwn}>
          <input aria-label="Answer in your own words" placeholder="Or answer in your own words" maxlength={2000} value={own()} onInput={(e) => setOwn(e.currentTarget.value)} />
          <div class="own-actions">
            <button type="button" class="ghost" onClick={props.onDismiss}>
              Dismiss
            </button>
            <button type="submit" class="ghost" disabled={!own().trim()}>
              Send
            </button>
          </div>
        </form>
      </Show>
    </article>
  );
}
