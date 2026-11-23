import { createEffect, createMemo, createSignal, flush, For, Match, onCleanup, Show, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import BotAvatar, { type Mood } from "./BotAvatar";
import { api, changesFor, errorText, PROVIDER_NAMES, type Agent, type Message } from "./api";
import type { ChatStore } from "./chatStore";
import ModelPicker from "./ModelPicker";
import { effortName, runningModelName } from "./models";
import {
  CheckIcon,
  ChevronIcon,
  CloseIcon,
  ExpandIcon,
  ComposeIcon,
  CopyIcon,
  EditIcon,
  FileIcon,
  GearIcon,
  GlobeIcon,
  LockIcon,
  LockOpenIcon,
  MicIcon,
  SearchIcon,
  SendIcon,
  StopIcon,
  TerminalIcon,
  WrenchIcon,
} from "./icons";
import { openLinks, renderMarkdown } from "./markdown";
import "./chat.css";
import {
  buildRows,
  classifyTool,
  dominantKind,
  formatDuration,
  summarizeTools,
  turnInfo,
  workspaceRelative,
  type Row,
  type ToolKind,
  type TurnInfo,
} from "./timeline";


const TOOL_ICONS: Record<ToolKind, () => JSX.Element> = {
  command: TerminalIcon,
  edit: EditIcon,
  read: FileIcon,
  search: SearchIcon,
  web: GlobeIcon,
  tool: WrenchIcon,
};

const clock = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

function CopyButton(props: { text: () => string; label: string }) {
  const [copied, setCopied] = createSignal(false);
  let timer: number | undefined;
  onCleanup(() => clearTimeout(timer));
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.text());
      setCopied(true);
      clearTimeout(timer);
      timer = window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };
  return (
    <button class="mini-button" aria-label={copied() ? "Copied" : props.label} title={copied() ? "Copied" : props.label} onClick={copy}>
      {copied() ? <CheckIcon /> : <CopyIcon />}
    </button>
  );
}

function ToolGroup(props: { messages: Message[]; workspace: string }) {
  const [open, setOpen] = createSignal(false);
  const entries = createMemo(() => props.messages.map((m) => classifyTool(workspaceRelative(m.text, props.workspace))));
  const single = () => entries().length === 1;
  const Icon = () => TOOL_ICONS[dominantKind(entries())]();
  return (
    <div class="row tools" data-slot="message" data-role="tools">
      <Show
        when={!single()}
        fallback={
          <div class="tool-line" title={props.messages[0].text}>
            <Icon />
            <span>{entries()[0].label}</span>
          </div>
        }
      >
        <button class="tool-line tool-toggle" aria-expanded={open() ? "true" : "false"} onClick={() => setOpen(!open())}>
          <Icon />
          <span>{summarizeTools(entries())}</span>
          <span class={["chevron", { open: open() }]}>
            <ChevronIcon />
          </span>
        </button>
        <Show when={open()}>
          <ul class="tool-list">
            <For each={entries()}>
              {(entry, index) => (
                <li title={props.messages[index()].text}>
                  {TOOL_ICONS[entry.kind]()}
                  <span>{entry.label}</span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </div>
  );
}

function AssistantRow(props: { message: Message; turn: () => TurnInfo | undefined }) {
  const m = () => props.message;
  const streaming = () => m().status === "streaming";
  const html = createMemo(() => (streaming() ? "" : renderMarkdown(m().text)));
  const meta = () => {
    const turn = props.turn();
    if (!turn?.endsTurn || streaming()) return null;
    const finished = m().updatedAt ?? m().createdAt;
    const note = m().status === "stopped" ? "Stopped" : m().status === "interrupted" ? "Cut off when Grimoire closed" : null;
    return { time: clock(finished), took: formatDuration(finished - turn.startedAt), note };
  };
  return (
    <div class="row assistant" data-slot="message" data-role="assistant">
      <Show when={!streaming()} fallback={<div class="plain">{m().text.trimEnd()}<span class="caret" aria-hidden="true" /></div>}>
        <div class="md" innerHTML={html()} onClick={openLinks} />
      </Show>
      <Show when={meta()}>
        {(info) => (
          <div class="meta">
            <span>{info().note ?? `Worked for ${info().took}`}</span>
            <span>{info().time}</span>
            <CopyButton text={() => m().text} label="Copy reply" />
          </div>
        )}
      </Show>
    </div>
  );
}

function UserRow(props: { message: Message }) {
  const m = () => props.message;
  return (
    <div class="row user" data-slot="message" data-role="user">
      <Show when={m().source}>
        <div class="user-source">{m().source}</div>
      </Show>
      <div class="user-bubble">{m().text}</div>
      <div class="meta end">
        <Show when={m().status === "queued" || m().status === "cancelled"}>
          <span class="status">{m().status === "queued" ? "Queued, runs next" : "Cancelled"}</span>
        </Show>
        <span class="hover-only">{clock(m().createdAt)}</span>
        <span class="hover-only">
          <CopyButton text={() => m().text} label="Copy message" />
        </span>
      </div>
    </div>
  );
}

function Working(props: { since: number | null }) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(timer));
  return (
    <div class="row working" role="status">
      <span class="pulse" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
      <span>{props.since ? `Working for ${formatDuration(now() - props.since)}` : "Working"}</span>
    </div>
  );
}

interface SpeechRecognitionLike {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: { results: ArrayLike<ArrayLike<{ transcript: string }>> }) => void) | null;
  onerror: ((event: { error: string }) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
}

const SpeechRecognition = (window as unknown as { webkitSpeechRecognition?: new () => SpeechRecognitionLike })
  .webkitSpeechRecognition;

const DICTATION_ERRORS: Record<string, string> = {
  "not-allowed": "Grimoire needs microphone and speech recognition access. Allow both in System Settings, Privacy & Security.",
  "service-not-allowed": "Speech recognition is turned off. Turn on Siri or Dictation in System Settings.",
  "audio-capture": "No microphone was found.",
  network: "Speech recognition needs an internet connection.",
};

export default function Chat(props: {
  agent: Agent;
  busy: boolean;
  mood?: Mood;
  store: ChatStore;
  onEdit: () => void;
  onAgentUpdated: (agent: Agent) => void;
  compact?: { onClose: () => void; onOpenMain: () => void };
}) {
  const [draft, setDraft] = createSignal("");
  const [actionError, setActionError] = createSignal("");
  const [listening, setListening] = createSignal(false);
  let recognition: SpeechRecognitionLike | undefined;
  let list!: HTMLDivElement;
  let input!: HTMLTextAreaElement;
  let stickToBottom = true;

  const messages = () => props.store.messages;
  const lastText = () => messages()[messages().length - 1]?.text;
  let rowCache = new Map<string, Row>();
  const rows = createMemo(() => {
    const next = buildRows(messages(), rowCache);
    rowCache = new Map(next.map((row) => [row.key, row]));
    return next;
  });
  const turns = createMemo(() => turnInfo(messages()));
  const workingSince = () => {
    for (let i = messages().length - 1; i >= 0; i--) {
      const m = messages()[i];
      if (m.role === "user" && m.status !== "queued") return m.createdAt;
    }
    return null;
  };
  const folder = () => props.agent.workspace.split("/").filter(Boolean).pop() ?? props.agent.workspace;
  const [picking, setPicking] = createSignal(false);
  const modelLabel = () => runningModelName(props.agent);
  const pickModel = (model: string, effort: string) =>
    run(async () => props.onAgentUpdated(await api.updateAgent(props.agent.id, changesFor(props.agent, { model, effort }))));

  createEffect(
    () => props.agent.id,
    () => {
      stickToBottom = true;
      setActionError("");
      input?.focus();
    },
  );

  createEffect(
    () => [messages().length, lastText(), props.busy],
    () => {
      if (stickToBottom) list.scrollTop = list.scrollHeight;
    },
  );

  const run = async (action: () => Promise<unknown>) => {
    setActionError("");
    try {
      await action();
    } catch (e) {
      setActionError(errorText(e));
    }
  };

  const resize = () => {
    flush(); // Measure the textarea after the draft change reaches the DOM.
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 240)}px`;
  };

  const stopDictation = () => recognition?.stop();
  onCleanup(stopDictation);

  const toggleDictation = () => {
    if (!SpeechRecognition) return;
    if (listening()) return stopDictation();
    setActionError("");
    const prefix = draft().trim() ? `${draft().trimEnd()} ` : "";
    recognition = new SpeechRecognition();
    recognition.continuous = true;
    recognition.interimResults = true;
    recognition.lang = navigator.language;
    recognition.onresult = (event) => {
      setDraft(prefix + Array.from(event.results, (result) => result[0].transcript).join(""));
      resize();
    };
    recognition.onerror = (event) => {
      if (event.error !== "aborted" && event.error !== "no-speech") {
        setActionError(DICTATION_ERRORS[event.error] ?? `Voice input stopped (${event.error}).`);
      }
    };
    recognition.onend = () => {
      setListening(false);
      input?.focus();
    };
    recognition.start();
    setListening(true);
  };

  const send = () => {
    stopDictation();
    const text = draft().trim();
    if (!text) return;
    setDraft("");
    resize();
    stickToBottom = true;
    void run(() =>
      api.sendMessage(props.agent.id, text).catch((e) => {
        setDraft(text);
        throw e;
      }),
    );
  };

  const toggleAccess = () =>
    run(async () =>
      props.onAgentUpdated(
        await api.updateAgent(props.agent.id, changesFor(props.agent, { access: props.agent.access === "full" ? "workspace" : "full" })),
      ),
    );

  const loadOlder = async () => {
    const before = list.scrollHeight;
    await run(props.store.loadOlder);
    flush();
    list.scrollTop += list.scrollHeight - before;
  };

  return (
    <section class={["chat", { compact: !!props.compact }]} data-slot="chat">
      <header class="chat-bar" data-slot="chat-header" data-tauri-drag-region>
        <div class="crumbs" data-tauri-drag-region>
          <BotAvatar avatar={props.agent.avatar} size={20} fluid working={props.busy} mood={props.mood} />
          <span class="crumb-name">{props.agent.name}</span>
          <Show when={!props.compact}>
            <span class="crumb-sep" aria-hidden="true">
              /
            </span>
            <button class="crumb-link" title={`Open ${props.agent.workspace} in Finder`} onClick={() => run(() => api.openWorkspace(props.agent.id))}>
              {folder()}
            </button>
          </Show>
        </div>
        <div class="bar-actions">
          <Show when={props.compact}>
            {(compact) => (
              <button class="icon-button" aria-label="Open in Grimoire" title="Open in Grimoire" onClick={compact().onOpenMain}>
                <ExpandIcon />
              </button>
            )}
          </Show>
          <button
            class="icon-button"
            aria-label="New chat"
            title="New chat"
            disabled={props.busy}
            onClick={() => run(() => api.newChat(props.agent.id).then(props.store.onMessage))}
          >
            <ComposeIcon />
          </button>
          <Show
            when={props.compact}
            fallback={
              <button class="icon-button" aria-label="Familiar settings" title="Familiar settings" onClick={props.onEdit}>
                <GearIcon />
              </button>
            }
          >
            {(compact) => (
              <button class="icon-button" aria-label="Close" title="Close (Esc)" onClick={compact().onClose}>
                <CloseIcon />
              </button>
            )}
          </Show>
        </div>
      </header>

      <div
        class="timeline"
        ref={list}
        onScroll={() => {
          stickToBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 80;
        }}
      >
        <div class="timeline-column">
          <Show when={props.store.hasOlder()}>
            <button class="mini-link load-older" onClick={loadOlder}>
              Load earlier messages
            </button>
          </Show>
          <Show when={!messages().length && !props.busy}>
            <div class="chat-empty">
              <BotAvatar avatar={props.agent.avatar} size={64} fluid />
              <p>Send {props.agent.name} a message to start.</p>
            </div>
          </Show>
          <For each={rows()}>
            {(row) => (
              <Switch>
                <Match when={row.kind === "user" && row}>{(r) => <UserRow message={(r() as Extract<Row, { kind: "user" }>).message} />}</Match>
                <Match when={row.kind === "assistant" && row}>
                  {(r) => {
                    const message = (r() as Extract<Row, { kind: "assistant" }>).message;
                    return <AssistantRow message={message} turn={() => turns().get(message.id)} />;
                  }}
                </Match>
                <Match when={row.kind === "tools" && row}>
                  {(r) => <ToolGroup messages={(r() as Extract<Row, { kind: "tools" }>).messages} workspace={props.agent.workspace} />}
                </Match>
                <Match when={row.kind === "error" && row}>{(r) => <div class="row error-box">{(r() as Extract<Row, { kind: "error" }>).message.text}</div>}</Match>
                <Match when={row.kind === "divider"}>
                  <div class="row divider" role="separator">
                    <span>New chat</span>
                  </div>
                </Match>
              </Switch>
            )}
          </For>
          <Show when={props.busy}>
            <Working since={workingSince()} />
          </Show>
        </div>
      </div>

      <footer class="composer-dock" data-slot="composer">
        <Show when={actionError()}>
          <p class="error-box composer-error" role="alert">
            {actionError()}
          </p>
        </Show>
        <div class="composer-card">
          <textarea
            ref={input}
            rows={1}
            aria-label={`Message ${props.agent.name}`}
            placeholder={props.busy ? "Queue a follow-up. It runs after this task." : `Ask ${props.agent.name} anything`}
            value={draft()}
            onInput={(e) => {
              setDraft(e.currentTarget.value);
              resize();
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                send();
              }
            }}
          />
          <div class="composer-footer">
            <button
              class="chip"
              aria-haspopup="menu"
              aria-expanded={picking() ? "true" : "false"}
              title={props.agent.activeModel ? `Running ${props.agent.activeModel}` : "Choose a model"}
              onClick={() => setPicking(!picking())}
            >
              <span class="chip-provider">{PROVIDER_NAMES[props.agent.provider]}</span>
              <span>{modelLabel()}</span>
              <Show when={props.agent.effort}>{(effort) => <span class="chip-effort">{effortName(effort())}</span>}</Show>
            </button>
            <Show when={picking()}>
              <ModelPicker agent={props.agent} onPick={pickModel} onClose={() => setPicking(false)} />
            </Show>
            <span class="chip-sep" aria-hidden="true" />
            <button
              class={["chip access-chip", { warn: props.agent.access === "full" }]}
              title={props.agent.access === "full" ? "Full access. Click for workspace only." : "Workspace only. Click for full access."}
              disabled={props.busy}
              onClick={toggleAccess}
            >
              {props.agent.access === "full" ? <LockOpenIcon /> : <LockIcon />}
              <span>{props.agent.access === "full" ? "Full access" : "Workspace only"}</span>
            </button>
            <span class="spacer" />
            <Show when={SpeechRecognition}>
              <button
                class={["icon-button mic", { listening: listening() }]}
                aria-label={listening() ? "Stop dictation" : "Dictate a message"}
                aria-pressed={listening() ? "true" : "false"}
                title={listening() ? "Stop dictation" : "Dictate a message"}
                onClick={toggleDictation}
              >
                <MicIcon />
              </button>
            </Show>
            <Show
              when={props.busy && !draft().trim()}
              fallback={
                <button class="round-button send" aria-label="Send" title="Send" disabled={!draft().trim()} onClick={send}>
                  <SendIcon />
                </button>
              }
            >
              <button class="round-button stop" aria-label="Stop" title="Stop" onClick={() => run(() => api.stopTurn(props.agent.id))}>
                <StopIcon />
              </button>
            </Show>
          </div>
        </div>
      </footer>
    </section>
  );
}
