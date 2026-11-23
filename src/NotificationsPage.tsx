import { createSignal, For, onSettled, Show } from "solid-js";
import { api, errorText, type NotificationPrefs } from "./api";
import { Page, Toggle } from "./ui";

const DEFAULTS: NotificationPrefs = { replies: true, approvals: true, runs: true, backgroundOnly: true };

const ROWS: { key: keyof NotificationPrefs; title: string; detail: string }[] = [
  { key: "replies", title: "Finished replies", detail: "When a familiar finishes or fails a turn, with the first line of its reply." },
  { key: "approvals", title: "Approvals", detail: "When a familiar needs your OK for a command or a vault item." },
  { key: "runs", title: "Rituals and spells", detail: "When a ritual runs, and when a spell finishes or fails." },
  { key: "backgroundOnly", title: "Only while Grimoire is in the background", detail: "Stay quiet while the Grimoire window is in front." },
];

export default function NotificationsPage() {
  const [prefs, setPrefs] = createSignal<NotificationPrefs>(DEFAULTS);
  const [error, setError] = createSignal("");
  onSettled(() => {
    api.notificationSettings().then(
      (saved) => setPrefs({ ...DEFAULTS, ...saved }),
      (e) => setError(errorText(e)),
    );
  });

  const change = async (key: keyof NotificationPrefs, on: boolean) => {
    const previous = prefs();
    const next = { ...previous, [key]: on };
    setPrefs(next);
    setError("");
    try {
      await api.saveNotificationSettings(next);
    } catch (e) {
      setPrefs(previous);
      setError(errorText(e));
    }
  };

  return (
    <Page title="Notifications">
      <div class="settings">
        <div class="list">
          <For each={ROWS}>
            {(row) => (
              <div class="list-row settings-row">
                <div class="list-main">
                  <div class="list-title">{row.title}</div>
                  <p class="settings-detail">{row.detail}</p>
                </div>
                <Toggle label={row.title} checked={prefs()[row.key]} onChange={(on) => void change(row.key, on)} />
              </div>
            )}
          </For>
        </div>
        <p class="hint settings-footnote">
          Click a notification to open that familiar. Approvals also have Allow and Deny buttons. macOS asks once whether Grimoire may
          notify you; change your answer in System Settings, Notifications.
        </p>
        <Show when={error()}>
          <div class="error-box settings-error">{error()}</div>
        </Show>
      </div>
    </Page>
  );
}
