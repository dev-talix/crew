import { createSignal, For, Show } from "solid-js";
import { errorText } from "./api";

export type Tab = "inbox" | "drafts" | "chat";
export type Connection = "connecting" | "online" | "offline";

const CONNECTION_LABEL: Record<Connection, string> = {
  connecting: "Connecting",
  online: "Connected",
  offline: "Can't reach your Mac. Retrying.",
};

export function TopBar(props: { mac: string; connection: Connection; onSettings: () => void }) {
  return (
    <header class="topbar">
      <div class="topbar-title">
        <span class="wordmark">Grimoire</span>
        <span class="topbar-mac">
          <span class={`dot ${props.connection}`} role="img" aria-label={CONNECTION_LABEL[props.connection]} />
          {props.connection === "offline" ? "Offline" : props.mac || "Your Mac"}
        </span>
      </div>
      <button class="icon-button" aria-label="Settings" onClick={props.onSettings}>
        <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
          <path d="M4 6h12M4 10h12M4 14h12" />
        </svg>
      </button>
      <Show when={props.connection === "offline"}>
        <p class="offline-line" role="status">
          {CONNECTION_LABEL.offline} Check that Tailscale is on.
        </p>
      </Show>
    </header>
  );
}

const TABS: { id: Tab; label: string; icon: string }[] = [
  { id: "inbox", label: "Inbox", icon: "M3 12.5 5.2 4.6a1 1 0 0 1 1-.7h7.6a1 1 0 0 1 1 .7l2.2 7.9V16a.7.7 0 0 1-.7.7H3.7A.7.7 0 0 1 3 16zM3 12.5h4l1 2h4l1-2h4" },
  { id: "drafts", label: "Drafts", icon: "M4 4.5h12v9H8l-4 3zM7 8h6M7 10.5h4" },
  { id: "chat", label: "Chat", icon: "M10 3.5c3.9 0 7 2.6 7 5.8s-3.1 5.8-7 5.8c-.8 0-1.6-.1-2.3-.3L4 16.5l.9-3C3.7 12.4 3 10.9 3 9.3 3 6.1 6.1 3.5 10 3.5z" },
];

export function TabBar(props: { tab: Tab; inbox: number; drafts: number; onTab: (tab: Tab) => void }) {
  const count = (id: Tab) => (id === "inbox" ? props.inbox : id === "drafts" ? props.drafts : 0);
  return (
    <nav class="tabbar" aria-label="Sections">
      <For each={TABS}>
        {(t) => (
          <button
            class={["tab", { active: props.tab === t.id }]}
            aria-current={props.tab === t.id ? "page" : undefined}
            aria-label={count(t.id) ? `${t.label}, ${count(t.id)} waiting` : t.label}
            onClick={() => props.onTab(t.id)}
          >
            <span class="tab-icon">
              <svg width="22" height="22" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true">
                <path d={t.icon} />
              </svg>
              <Show when={count(t.id)}>
                <span class="badge">{count(t.id)}</span>
              </Show>
            </span>
            <span>{t.label}</span>
          </button>
        )}
      </For>
    </nav>
  );
}

export function Toast(props: { text: string; onOpen: () => void }) {
  return (
    <button class="toast" role="status" onClick={props.onOpen}>
      {props.text}
    </button>
  );
}

export function Settings(props: {
  mac: string;
  /** The notification permission, or null when this browser can't show notifications here. */
  notify: NotificationPermission | null;
  onTurnOnAlerts: () => void;
  onUnpair: () => Promise<void>;
  onClose: () => void;
}) {
  const [error, setError] = createSignal("");
  const [leaving, setLeaving] = createSignal(false);
  const unpair = async () => {
    setLeaving(true);
    setError("");
    try {
      await props.onUnpair();
    } catch (e) {
      setError(errorText(e));
      setLeaving(false);
    }
  };
  return (
    <div class="sheet-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="sheet" role="dialog" aria-modal="true" aria-label="Settings">
        <div class="sheet-head">
          <h2>Settings</h2>
          <button class="ghost" onClick={props.onClose}>
            Done
          </button>
        </div>
        <Show when={props.notify}>
          {(permission) => (
            <section class="sheet-row">
              <div>
                <p class="row-title">Alerts</p>
                <p class="row-note">
                  {permission() === "granted"
                    ? "On. You'll get an alert when the page is in the background."
                    : permission() === "denied"
                      ? "Blocked in your browser's settings."
                      : "Get an alert when a familiar needs you and this page is in the background."}
                </p>
              </div>
              <Show when={permission() === "default"}>
                <button class="primary" onClick={props.onTurnOnAlerts}>
                  Turn on alerts
                </button>
              </Show>
            </section>
          )}
        </Show>
        <section class="sheet-row">
          <div>
            <p class="row-title">Paired with {props.mac || "your Mac"}</p>
            <p class="row-note">Unpairing signs this phone out. You'll need a new code from your Mac to come back.</p>
          </div>
          <button class="ghost danger" disabled={leaving()} onClick={() => void unpair()}>
            Unpair this phone
          </button>
        </section>
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>
      </div>
    </div>
  );
}
