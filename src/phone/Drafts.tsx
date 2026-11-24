import { createSignal, createStore, For, Show } from "solid-js";
import { draftTitle, mailtoUrl, parseRecipients } from "../draftText";
import { api, errorText, type Draft, type Familiar } from "./api";
import { FamiliarFace } from "./Face";
import { timeAgo } from "./logic";

/**
 * Copies text and says whether it worked. The phone opens Grimoire over plain HTTP on the tailnet,
 * where browsers leave out navigator.clipboard, so this falls back to a selected text area.
 */
function copyText(text: string): boolean {
  if (window.isSecureContext && navigator.clipboard) {
    void navigator.clipboard.writeText(text);
    return true;
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.append(area);
  area.select();
  area.setSelectionRange(0, text.length);
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    area.remove();
  }
}

type Edit = { to: string; subject: string; body: string };

/** Drafts familiars wrote for the person to send. The phone opens them in Mail or copies them; it never sends. */
export function DraftList(props: { drafts: Draft[]; familiars: Familiar[]; reload: () => Promise<void> }) {
  const [showRecent, setShowRecent] = createSignal(false);
  // Edits live here by draft id, so a reload when another draft arrives keeps what the person typed.
  const [edits, setEdits] = createStore<Record<string, Edit>>({});
  const [hint, setHint] = createSignal<{ id: string; text: string; error?: boolean } | null>(null);
  const [working, setWorking] = createSignal<string | null>(null);
  const waiting = () => props.drafts.filter((d) => d.state === "draft").sort((a, b) => b.createdAt - a.createdAt);
  const closed = () => props.drafts.filter((d) => d.state !== "draft").sort((a, b) => (b.closedAt ?? 0) - (a.closedAt ?? 0));
  const familiar = (id: string) => props.familiars.find((f) => f.id === id);

  const edit = (d: Draft): Edit => edits[d.id] ?? { to: d.to.join(", "), subject: d.subject ?? "", body: d.body };
  const change = (d: Draft, patch: Partial<Edit>) =>
    setEdits((all) => {
      all[d.id] = { ...edit(d), ...patch };
    });
  const say = (d: Draft, text: string, error = false) => setHint({ id: d.id, text, error });

  const run = async (d: Draft, action: () => Promise<void>) => {
    setHint(null);
    try {
      await action();
    } catch (e) {
      say(d, errorText(e), true);
    }
  };
  const copy = (text: string) => copyText(text);

  const openInMail = (d: Draft) =>
    run(d, async () => {
      const e = edit(d);
      const { url, bodyIncluded } = mailtoUrl(parseRecipients(e.to), e.subject, e.body);
      const copied = bodyIncluded || copy(e.body);
      window.location.href = url;
      say(
        d,
        bodyIncluded
          ? "Opened in Mail. Mark it sent once you send it."
          : copied
            ? "The message is too long for a Mail link, so it's on your clipboard. Paste it in."
            : "The message is too long for a Mail link, and copying didn't work. Select the message above and copy it.",
      );
    });
  const copyBody = (d: Draft) =>
    run(d, async () => {
      const copied = copy(edit(d).body);
      say(d, copied ? "Copied the message." : "Copying didn't work. Select the message above and copy it.", !copied);
    });
  const openSlack = (d: Draft) =>
    run(d, async () => {
      const copied = copy(edit(d).body);
      window.location.href = "slack://open";
      say(d, copied ? "Copied, and opened Slack. Paste it where it goes." : "Opened Slack. Copying didn't work, so copy the message above first.");
    });
  const close = (d: Draft, sent: boolean) =>
    run(d, async () => {
      setWorking(d.id);
      try {
        const e = edit(d);
        if (sent) await api.send(d.id, { to: parseRecipients(e.to), subject: d.channel === "email" ? e.subject : null, body: e.body });
        else await api.discard(d.id);
        setEdits((all) => {
          delete all[d.id];
        });
        await props.reload();
      } finally {
        setWorking(null);
      }
    });

  return (
    <section class="page" aria-label="Drafts">
      <h1 class="page-title">Drafts</h1>
      <Show when={waiting().length === 0}>
        <div class="empty">
          <p class="empty-title">Nothing to send</p>
          <p>When a familiar writes an email or a Slack message for you, it waits here.</p>
        </div>
      </Show>
      <div class="cards">
        <For each={waiting()}>
          {(d) => (
            <article class="card" aria-label={draftTitle(d.channel, d.to)} style={familiar(d.agentId) ? { "--agent": familiar(d.agentId)!.avatar.color } : undefined}>
              <div class="card-who">
                <FamiliarFace familiar={familiar(d.agentId)} size={24} />
                <span>{familiar(d.agentId)?.name ?? "A familiar"}</span>
                <span class="card-when">{timeAgo(d.createdAt)}</span>
              </div>
              <h2 class="card-title">{draftTitle(d.channel, parseRecipients(edit(d).to))}</h2>
              <Show when={d.note}>
                <p class="card-detail">{d.note}</p>
              </Show>
              <label class="field">
                <span>{d.channel === "post" ? "Where" : "To"}</span>
                <input value={edit(d).to} onInput={(e) => change(d, { to: e.currentTarget.value })} spellcheck={false} autocapitalize="off" />
              </label>
              <Show when={d.channel === "email"}>
                <label class="field">
                  <span>Subject</span>
                  <input value={edit(d).subject} onInput={(e) => change(d, { subject: e.currentTarget.value })} />
                </label>
              </Show>
              <label class="field">
                <span>Message</span>
                <textarea rows={6} value={edit(d).body} onInput={(e) => change(d, { body: e.currentTarget.value })} />
              </label>
              <Show when={hint()?.id === d.id}>
                <p class={hint()!.error ? "error-box" : "hint"} role={hint()!.error ? "alert" : "status"}>
                  {hint()!.text}
                </p>
              </Show>
              <div class="draft-actions">
                <Show when={d.channel === "email"}>
                  <button class="ghost" onClick={() => void openInMail(d)}>
                    Open in Mail
                  </button>
                </Show>
                <Show when={d.channel !== "email"}>
                  <button class="ghost" onClick={() => void copyBody(d)}>
                    Copy
                  </button>
                </Show>
                <Show when={d.channel === "slack"}>
                  <button class="ghost" onClick={() => void openSlack(d)}>
                    Open Slack
                  </button>
                </Show>
              </div>
              <div class="draft-actions">
                <button class="ghost danger" disabled={working() === d.id} onClick={() => void close(d, false)}>
                  Discard
                </button>
                <button class="primary" disabled={working() === d.id} onClick={() => void close(d, true)}>
                  Mark sent
                </button>
              </div>
            </article>
          )}
        </For>
      </div>
      <Show when={closed().length > 0}>
        <button class="recent-toggle" aria-expanded={showRecent() ? "true" : "false"} onClick={() => setShowRecent((v) => !v)}>
          Recent <span class="recent-count">{closed().length}</span>
          <span class="chevron" aria-hidden="true">
            {showRecent() ? "▴" : "▾"}
          </span>
        </button>
        <Show when={showRecent()}>
          <ul class="recent">
            <For each={closed()}>
              {(d) => (
                <li>
                  <span class="recent-title">{draftTitle(d.channel, d.to)}</span>
                  <span class="recent-meta">
                    {d.state === "sent" ? "Sent" : "Discarded"}
                    {d.closedAt ? ` ${timeAgo(d.closedAt)}` : ""}
                  </span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </section>
  );
}
