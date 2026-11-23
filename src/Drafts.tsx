import { createSignal, createStore, For, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { inkFor } from "./avatar";
import { api, errorText, onEvent, onMountAsync, relativeTime, type Agent, type Draft } from "./api";
import { draftTitle, mailtoUrl, parseRecipients } from "./draftText";
import "./drafts.css";

type Edit = { to: string; subject: string; body: string };

/**
 * Messages familiars drafted for the person to send, in the bottom corner beside the sidebar. Grimoire
 * never sends them: the person opens them in Mail or copies them, then marks them sent or discards them.
 * Draft text is always shown as plain text.
 */
export default function Drafts(props: { agents: Agent[] }) {
  const [drafts, setDrafts] = createSignal<Draft[]>([]);
  const [open, setOpen] = createSignal(false);
  const [recent, setRecent] = createSignal(false);
  const [error, setError] = createSignal("");
  const [hint, setHint] = createSignal<{ id: string; text: string } | null>(null);
  // Edits live here by draft id, so a reload when another draft arrives keeps what the person typed.
  const [edits, setEdits] = createStore<Record<string, Edit>>({});
  const waiting = () => drafts().filter((d) => d.state === "draft");
  const closed = () => drafts().filter((d) => d.state !== "draft");

  const load = async () => {
    const before = new Set(waiting().map((d) => d.id));
    const list = await api.listDrafts();
    setDrafts(list);
    if (list.some((d) => d.state === "draft" && !before.has(d.id))) {
      setRecent(false);
      setOpen(true);
    }
  };

  onMountAsync(async (keep) => {
    keep(await onEvent("drafts_changed", () => void load().catch(() => {})));
    // A deleted familiar takes its drafts with it.
    keep(await onEvent("agents_changed", () => void load().catch(() => {})));
    await load().catch((e) => setError(errorText(e)));
  });

  const edit = (draft: Draft): Edit => edits[draft.id] ?? { to: draft.to.join(", "), subject: draft.subject ?? "", body: draft.body };
  const change = (draft: Draft, patch: Partial<Edit>) =>
    setEdits((all) => {
      all[draft.id] = { ...edit(draft), ...patch };
    });
  const say = (draft: Draft, text: string) => setHint({ id: draft.id, text });

  const run = async (action: () => Promise<unknown>) => {
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const copy = (draft: Draft) =>
    run(async () => {
      await navigator.clipboard.writeText(edit(draft).body);
      say(draft, "Copied the message.");
    });

  const openInMail = (draft: Draft) =>
    run(async () => {
      const e = edit(draft);
      const { url, bodyIncluded } = mailtoUrl(parseRecipients(e.to), e.subject, e.body);
      if (!bodyIncluded) await navigator.clipboard.writeText(e.body);
      await api.openDraftLink(draft.id, url);
      say(draft, bodyIncluded ? "Opened in Mail. Mark it sent once you send it." : "The message is too long for a Mail link, so it's on your clipboard. Paste it in.");
    });

  const openSlack = (draft: Draft) =>
    run(async () => {
      await navigator.clipboard.writeText(edit(draft).body);
      await api.openDraftLink(draft.id, "slack://open");
      say(draft, "Copied, and opened Slack. Paste it where it goes.");
    });

  const close = (draft: Draft, sent: boolean) =>
    run(async () => {
      const e = edit(draft);
      await api.closeDraft(draft.id, sent, sent ? { to: parseRecipients(e.to), subject: draft.channel === "email" ? e.subject : null, body: e.body } : null);
      setEdits((all) => {
        delete all[draft.id];
      });
      await load();
    });

  const agentOf = (draft: Draft) => props.agents.find((a) => a.id === draft.agentId);

  return (
    <Show when={drafts().length > 0}>
      <div class="drafts" role="region" aria-label="Drafts from familiars">
        <Show when={open()}>
          <div class="drafts-panel">
            <div class="drafts-head">
              <span class="drafts-heading">{recent() ? "Recent drafts" : "Drafts to send"}</span>
              <span class="spacer" />
              <button class="ghost small" onClick={() => setRecent((r) => !r)}>
                {recent() ? "Waiting" : "Recent"}
              </button>
              <button class="ghost small" onClick={() => setOpen(false)}>
                Hide
              </button>
            </div>
            <Show when={error()}>
              <p class="error-box" role="alert">
                {error()}
              </p>
            </Show>
            <div class="drafts-list">
              <Show when={!recent()}>
                <Show when={waiting().length === 0}>
                  <p class="drafts-empty">Nothing waiting. Familiars' drafts show up here.</p>
                </Show>
                <For each={waiting()}>
                  {(draft) => {
                    const agent = () => agentOf(draft);
                    return (
                      <div class="draft" role="group" aria-label={draftTitle(draft.channel, draft.to)} style={agent() ? { "--agent": agent()!.avatar.color, "--agent-ink": inkFor(agent()!.avatar.color) } : undefined}>
                        <div class="draft-head">
                          <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={28} mood="idle" />}</Show>
                          <div>
                            <p class="draft-title">
                              {agent()?.name ?? "A familiar"} drafted {draft.channel === "email" ? "an email" : draft.channel === "slack" ? "a Slack message" : "a post"}
                            </p>
                            <Show when={draft.note}>
                              <p class="draft-note">{draft.note}</p>
                            </Show>
                          </div>
                        </div>
                        <label class="draft-field">
                          <span>{draft.channel === "post" ? "Where" : "To"}</span>
                          <input value={edit(draft).to} onInput={(e) => change(draft, { to: e.currentTarget.value })} spellcheck={false} />
                        </label>
                        <Show when={draft.channel === "email"}>
                          <label class="draft-field">
                            <span>Subject</span>
                            <input value={edit(draft).subject} onInput={(e) => change(draft, { subject: e.currentTarget.value })} />
                          </label>
                        </Show>
                        <textarea class="draft-body" aria-label="Message" value={edit(draft).body} onInput={(e) => change(draft, { body: e.currentTarget.value })} />
                        <Show when={hint()?.id === draft.id}>
                          <p class="draft-hint" role="status">
                            {hint()!.text}
                          </p>
                        </Show>
                        <div class="draft-actions">
                          <button class="ghost small" onClick={() => void close(draft, false)}>
                            Discard
                          </button>
                          <span class="spacer" />
                          <button class="ghost small" onClick={() => void copy(draft)}>
                            Copy
                          </button>
                          <Show when={draft.channel === "email"}>
                            <button class="ghost small" onClick={() => void openInMail(draft)}>
                              Open in Mail
                            </button>
                          </Show>
                          <Show when={draft.channel === "slack"}>
                            <button class="ghost small" onClick={() => void openSlack(draft)}>
                              Open Slack
                            </button>
                          </Show>
                          <button class="primary small" onClick={() => void close(draft, true)}>
                            Mark sent
                          </button>
                        </div>
                      </div>
                    );
                  }}
                </For>
              </Show>
              <Show when={recent()}>
                <Show when={closed().length === 0}>
                  <p class="drafts-empty">No sent or discarded drafts yet.</p>
                </Show>
                <For each={closed()}>
                  {(draft) => (
                    <details class="draft-row">
                      <summary>
                        <Show when={agentOf(draft)}>{(a) => <BotAvatar avatar={a().avatar} size={18} alive={false} />}</Show>
                        <span class="draft-row-title">{draft.subject ?? draftTitle(draft.channel, draft.to)}</span>
                        <span class="draft-row-state">
                          {draft.state === "sent" ? "Sent" : "Discarded"} {relativeTime(draft.closedAt ?? draft.createdAt)}
                        </span>
                      </summary>
                      <p class="draft-row-to">{draftTitle(draft.channel, draft.to)}</p>
                      <pre class="draft-row-body">{draft.body}</pre>
                    </details>
                  )}
                </For>
              </Show>
            </div>
          </div>
        </Show>
        <button class={["drafts-launcher", { waiting: waiting().length > 0 }]} aria-expanded={open() ? "true" : "false"} onClick={() => setOpen((o) => !o)}>
          {waiting().length ? `${waiting().length} ${waiting().length === 1 ? "draft" : "drafts"} to send` : "Drafts"}
        </button>
      </div>
    </Show>
  );
}
