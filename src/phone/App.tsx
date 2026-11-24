import { createEffect, createSignal, onSettled, Show } from "solid-js";
import { api, clearToken, token, Unauthorized, whenUnauthorized, type Draft, type Familiar, type InboxItem, type Message } from "./api";
import { ChatList, Conversation } from "./Chat";
import { DraftList } from "./Drafts";
import { InboxList } from "./Inbox";
import { applyChatEvent, backoff, draftAlert, inboxAlert, mergeHistory, pendingDrafts, replyAlert, titleText, type PhoneEvent } from "./logic";
import Pair from "./Pair";
import { Settings, TabBar, Toast, TopBar, type Connection, type Tab } from "./Shell";

export default function App() {
  const [paired, setPaired] = createSignal(!!token());
  whenUnauthorized(() => setPaired(false));
  return (
    <Show when={paired()} fallback={<Pair onPaired={() => setPaired(true)} />}>
      <Home onUnpaired={() => setPaired(false)} />
    </Show>
  );
}

const canNotify = () => "Notification" in window && window.isSecureContext;

function Home(props: { onUnpaired: () => void }) {
  const [mac, setMac] = createSignal("");
  const [connection, setConnection] = createSignal<Connection>("connecting");
  const [tab, setTab] = createSignal<Tab>("inbox");
  const [inbox, setInbox] = createSignal<InboxItem[]>([]);
  const [drafts, setDrafts] = createSignal<Draft[]>([]);
  const [familiars, setFamiliars] = createSignal<Familiar[]>([]);
  const [busy, setBusy] = createSignal<string[]>([]);
  const [openId, setOpenId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Message[]>([]);
  const [toast, setToast] = createSignal<{ text: string; tab: Tab; agentId?: string } | null>(null);
  const [settings, setSettings] = createSignal(false);
  const [notify, setNotify] = createSignal(canNotify() ? Notification.permission : "denied");

  // Plain variables, read and written synchronously inside the event loop.
  let seenInbox: Set<string> | null = null;
  let seenDrafts: Set<string> | null = null;
  let watching: string | null = null;
  let toastTimer: number | undefined;

  const nameOf = (id: string) => familiars().find((f) => f.id === id)?.name ?? "A familiar";

  const alert = (text: string | null, target: Tab, agentId?: string) => {
    if (!text) return;
    clearTimeout(toastTimer);
    setToast({ text, tab: target, agentId });
    toastTimer = window.setTimeout(() => setToast(null), 5000);
    if (document.hidden && canNotify() && Notification.permission === "granted") {
      try {
        new Notification("Grimoire", { body: text, tag: target });
      } catch {
        // Some mobile browsers only allow notifications from a service worker. The banner still shows.
      }
    }
  };

  const loadInbox = async () => {
    const list = await api.inbox();
    alert(inboxAlert(seenInbox, list, nameOf), "inbox");
    seenInbox = new Set([...(seenInbox ?? []), ...list.map((i) => i.id)]);
    setInbox(list);
  };
  const loadDrafts = async () => {
    const list = await api.drafts();
    alert(draftAlert(seenDrafts, list, nameOf), "drafts");
    seenDrafts = new Set([...(seenDrafts ?? []), ...list.map((d) => d.id)]);
    setDrafts(list);
  };
  const loadFamiliars = async () => {
    const { familiars: list, busy: running } = await api.familiars();
    setFamiliars(list);
    setBusy(running);
  };
  // Only the latest history request lands, so a slow older one can't replace a newer transcript.
  let historyRequest = 0;
  const loadHistory = async (id: string, merge: boolean) => {
    const request = ++historyRequest;
    const list = await api.messages(id);
    if (request !== historyRequest || openId() !== id) return;
    setMessages((live) => (merge ? mergeHistory(list, live) : list));
  };
  const loadMessages = async () => {
    const id = openId();
    if (id) await loadHistory(id, false);
  };
  const loadAll = () =>
    Promise.all([
      api.hello().then((h) => setMac(h.mac)),
      loadFamiliars(),
      loadInbox(),
      loadDrafts(),
      loadMessages(),
    ]);

  // An event says a list changed; a failed reload tries again, then waits for the next good poll,
  // so the change isn't lost.
  const stale = new Set<() => Promise<void>>();
  const retrying = async (load: () => Promise<void>) => {
    stale.delete(load);
    for (let attempt = 0; attempt < 4; attempt++) {
      try {
        return await load();
      } catch (e) {
        if (e instanceof Unauthorized) return;
        await new Promise((r) => setTimeout(r, backoff(attempt)));
      }
    }
    stale.add(load);
  };

  const apply = (event: PhoneEvent) => {
    if (event.kind === "message" || event.kind === "delta") {
      const id = openId();
      if (id) setMessages((list) => applyChatEvent(list, id, event));
    } else if (event.kind === "status") {
      const { agentId, busy: running } = event.payload;
      setBusy((list) => (running ? [...new Set([...list, agentId])] : list.filter((id) => id !== agentId)));
      alert(replyAlert(event, watching, nameOf), "chat", agentId);
    } else if (event.kind === "chat") {
      // The familiar switched chats on the Mac, so the open one shows its new chat.
      if (event.payload.agentId === openId()) void retrying(loadMessages);
    } else if (event.kind === "inbox") void retrying(loadInbox);
    else if (event.kind === "drafts") void retrying(loadDrafts);
    else if (event.kind === "familiars") void retrying(loadFamiliars);
  };

  onSettled(() => {
    const stop = new AbortController();
    // A phone that comes back from the background, or back online, retries at once instead of waiting out the backoff.
    let wake = () => {};
    const sleep = (ms: number) =>
      new Promise<void>((r) => {
        const timer = setTimeout(r, ms);
        wake = () => {
          clearTimeout(timer);
          r();
        };
      });
    const retry = () => document.visibilityState === "visible" && wake();
    document.addEventListener("visibilitychange", retry);
    window.addEventListener("online", retry);
    void (async () => {
      let seq: number | null = null;
      let epoch: string | null = null;
      let failures = 0;
      while (!stop.signal.aborted) {
        try {
          const batch = await api.events(seq, stop.signal);
          setConnection("online");
          failures = 0;
          for (const load of [...stale]) void retrying(load);
          if (seq === null || batch.reset || batch.epoch !== epoch) await loadAll();
          else batch.events.forEach(apply);
          seq = batch.seq;
          epoch = batch.epoch;
        } catch (e) {
          if (stop.signal.aborted || e instanceof Unauthorized) return;
          setConnection("offline");
          await sleep(backoff(failures++));
        }
      }
    })();
    return () => {
      stop.abort();
      document.removeEventListener("visibilitychange", retry);
      window.removeEventListener("online", retry);
      clearTimeout(toastTimer);
      document.title = titleText(0);
    };
  });

  createEffect(
    () => inbox().length,
    (n) => {
      document.title = titleText(n);
    },
  );

  // The chat the person is looking at, so its finished replies don't raise a banner.
  createEffect(
    () => (tab() === "chat" ? openId() : null),
    (id) => {
      watching = id;
    },
  );

  const openChat = (id: string | null) => {
    setMessages([]);
    setOpenId(id);
    setTab("chat");
    if (id) void loadHistory(id, true).catch(() => {});
  };

  const unpair = async () => {
    await api.unpair().catch((e) => {
      if (!(e instanceof Unauthorized)) throw e;
    });
    clearToken();
    props.onUnpaired();
  };

  const turnOnAlerts = async () => setNotify(await Notification.requestPermission());

  const open = () => familiars().find((f) => f.id === openId());

  return (
    <div class="phone">
      <TopBar mac={mac()} connection={connection()} onSettings={() => setSettings(true)} />
      <Show when={toast()}>
        {(t) => (
          <Toast
            text={t().text}
            onOpen={() => {
              setToast(null);
              if (t().agentId) openChat(t().agentId!);
              else setTab(t().tab);
            }}
          />
        )}
      </Show>
      <main class="phone-main">
        <Show when={tab() === "inbox"}>
          <InboxList items={inbox()} familiars={familiars()} setItems={setInbox} reload={() => loadInbox().catch(() => {})} />
        </Show>
        <Show when={tab() === "drafts"}>
          <DraftList drafts={drafts()} familiars={familiars()} reload={() => loadDrafts().catch(() => {})} />
        </Show>
        <Show when={tab() === "chat"}>
          <Show when={open()} fallback={<ChatList familiars={familiars()} busy={busy()} onOpen={openChat} />}>
            {(f) => (
              <Conversation
                familiar={f()}
                messages={messages()}
                busy={busy().includes(f().id)}
                onBack={() => setOpenId(null)}
                onSent={(m) => setMessages((list) => applyChatEvent(list, f().id, { kind: "message", payload: m }))}
              />
            )}
          </Show>
        </Show>
      </main>
      <TabBar
        tab={tab()}
        inbox={inbox().length}
        drafts={pendingDrafts(drafts()).length}
        onTab={(t) => {
          if (t === "chat" && tab() === "chat") setOpenId(null);
          setTab(t);
        }}
      />
      <Show when={settings()}>
        <Settings
          mac={mac()}
          notify={canNotify() ? notify() : null}
          onTurnOnAlerts={() => void turnOnAlerts()}
          onUnpair={unpair}
          onClose={() => setSettings(false)}
        />
      </Show>
    </div>
  );
}
