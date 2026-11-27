import { createSignal, For, Show } from "solid-js";
import { api, errorText, type BackgroundSettings, type BackgroundSwitch } from "./api";
import { Toggle } from "./ui";

const ROWS: { key: BackgroundSwitch; title: string; detail: string }[] = [
  {
    key: "runInBackground",
    title: "Run in the background",
    detail: "Closing the window keeps Grimoire running without a Dock icon. Open Grimoire again, or use the menu bar, to bring the window back.",
  },
  {
    key: "startAtLogin",
    title: "Start at login",
    detail: "Opens Grimoire in the background, with no window, when you log in to this Mac.",
  },
  {
    key: "keepAwake",
    title: "Keep this Mac awake while Grimoire runs",
    detail: "Stops the Mac from sleeping on its own so rituals and linked Macs keep working. The display can still sleep.",
  },
];

/** Settings, General: run Grimoire on an always-on Mac with no window. */
export function BackgroundSection() {
  // Null until the saved values load; the switches stay disabled until then and while a change saves.
  const [settings, setSettings] = createSignal<BackgroundSettings | null>(null);
  const [saving, setSaving] = createSignal<BackgroundSwitch | null>(null);
  const [error, setError] = createSignal("");
  api.backgroundSettings().then(setSettings, (e) => setError(errorText(e)));

  const change = async (key: BackgroundSwitch, on: boolean) => {
    if (saving() || !settings()) return;
    setSaving(key);
    setError("");
    try {
      setSettings(await api.setBackgroundSwitch(key, on));
    } catch (e) {
      setError(errorText(e));
      await api.backgroundSettings().then(setSettings, () => undefined);
    } finally {
      setSaving(null);
    }
  };

  return (
    <>
      <h2 class="section-title">Background</h2>
      <div class="list">
        <For each={ROWS}>
          {(row) => (
            <div class="list-row settings-row">
              <div class="list-main">
                <div class="list-title">{row.title}</div>
                <p class="settings-detail">{row.detail}</p>
              </div>
              <Toggle
                label={row.title}
                checked={settings()?.[row.key] ?? false}
                disabled={!settings() || saving() !== null}
                onChange={(on) => void change(row.key, on)}
              />
            </div>
          )}
        </For>
      </div>
      <p class="hint settings-footnote">
        For a Mac mini or Mac Studio that stays on. It needs a logged-in macOS session.
        <Show when={settings()?.launchedInBackground}> Grimoire started in the background at login, so closing the window keeps it running until you quit.</Show>
      </p>
      <Show when={settings()?.keepAwake && !settings()?.awakeHolding}>
        <div class="error-box settings-error">Keep awake is on, but caffeinate isn't running, so this Mac can still sleep.</div>
      </Show>
      <Show when={error()}>
        <div class="error-box settings-error">{error()}</div>
      </Show>
    </>
  );
}
