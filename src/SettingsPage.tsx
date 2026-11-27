import { getVersion } from "@tauri-apps/api/app";
import { createEffect, createSignal, For, Match, onSettled, Show, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import { api, createFetch, errorText, PROVIDER_NAMES, type Agent, type DockEdge, type Provider, type ProviderStatus, type Settings } from "./api";
import BotAvatar from "./BotAvatar";
import { Page, Segmented, Toggle } from "./ui";
import { ThemePicker } from "./ThemeEditor";
import { BackupSection } from "./ImportExport";
import { BackgroundSection } from "./BackgroundSettings";
import { appIcon, readMode, setAppIcon, setMode, type AppIconSetting, type ModeSetting } from "./themeStore";
import nightIcon from "./assets/icon-night.svg";
import paperIcon from "./assets/icon-paper.svg";
import { BarsIcon, BellIcon, ChannelIcon, CloseIcon, DownloadIcon, GearIcon, LinkIcon, PaletteIcon, PhoneIcon, PlugIcon, RefreshIcon, ShieldIcon, VaultIcon } from "./icons";
import ChannelsPage from "./ChannelsPage";
import ConnectorsPage from "./ConnectorsPage";
import LinkedMacsPage from "./LinkedMacsPage";
import PhonePage from "./PhonePage";
import { shortcutFromKey, shortcutLabel } from "./quick";
import NotificationsPage from "./NotificationsPage";
import UpdatesPage from "./UpdatesPage";
import UsagePage from "./UsagePage";
import PermissionsPage from "./PermissionsPage";
import VaultPage from "./VaultPage";
import "./settings.css";

type Billing = Settings["claudeBilling"];

const PROVIDERS: Provider[] = ["codex", "claude"];
const APP_ICONS: { value: AppIconSetting; label: string; art: string[] }[] = [
  { value: "auto", label: "Match appearance", art: [paperIcon, nightIcon] },
  { value: "paper", label: "Paper", art: [paperIcon] },
  { value: "night", label: "Night", art: [nightIcon] },
];

export function accountState(status: ProviderStatus | undefined): "loading" | "ok" | "unknown" | "off" {
  if (!status) return "loading";
  if (!status.installed || status.signedIn === false) return "off";
  return status.signedIn ? "ok" : "unknown";
}

export function accountSummary(status: ProviderStatus | undefined): string {
  if (!status) return "Checking…";
  if (!status.installed) return "Not installed";
  if (status.signedIn === false) return "Not signed in";
  if (status.signedIn) return status.method ? `Signed in with ${status.method}` : "Signed in";
  return "Sign-in status unknown";
}

// Renders `backticked` spans as inline code.
export function WithCode(props: { text: string }) {
  return <For each={props.text.split("`")}>{(part, i) => (i() % 2 ? <code>{part}</code> : part)}</For>;
}


function GeneralSettings(props: { agents: Agent[] }) {
  const [statuses, setStatuses] = createSignal<ProviderStatus[]>();
  const [checking, setChecking] = createSignal(false);
  const [statusError, setStatusError] = createSignal("");

  const checkAccounts = async () => {
    if (checking()) return;
    setChecking(true);
    setStatusError("");
    try {
      setStatuses(await api.providerStatus());
    } catch (e) {
      setStatusError(errorText(e));
    } finally {
      setChecking(false);
    }
  };
  onSettled(() => void checkAccounts());

  const [settings, setSettings] = createSignal<Settings>({
    claudeBilling: "subscription",
    dockEdge: "off",
    quickAskEnabled: true,
    quickAskShortcut: "CommandOrControl+Shift+Space",
  });
  const [billingError, setBillingError] = createSignal("");
  const [dockError, setDockError] = createSignal("");
  // Familiars left out of the dock; every other one shows, including new ones.
  const [dockHidden, setDockHidden] = createSignal<string[]>([]);
  api.dockHidden().then(setDockHidden, (e) => setDockError(errorText(e)));
  const showInDock = (id: string, show: boolean) => {
    const before = dockHidden();
    const next = show ? before.filter((other) => other !== id) : [...before, id];
    setDockHidden(next);
    setDockError("");
    api.setDockHidden(next).then(setDockHidden, (e) => {
      setDockHidden(before);
      setDockError(errorText(e));
    });
  };
  api.getSettings().then(setSettings, (e) => setBillingError(errorText(e)));
  const billing = () => settings().claudeBilling;

  // Saves run one at a time, each on what the last one left, so a refused shortcut can't ride along
  // with the next change. A refused save shows what Grimoire actually kept.
  let saving = Promise.resolve();
  const saveSetting = (patch: Partial<Settings>, setError: (message: string) => void) => {
    setSettings((current) => ({ ...current, ...patch }));
    setError("");
    saving = saving.then(async () => {
      try {
        setSettings(await api.saveSettings({ ...(await api.getSettings()), ...patch }));
        setError("");
      } catch (e) {
        setError(errorText(e));
        await api.getSettings().then(setSettings, () => undefined);
      }
    });
    return saving;
  };

  const changeBilling = (next: Billing) => {
    if (next !== billing()) void saveSetting({ claudeBilling: next }, setBillingError);
  };

  const changeDock = (next: DockEdge) => {
    if (next !== settings().dockEdge) void saveSetting({ dockEdge: next }, setDockError);
  };

  const [quickError, setQuickError] = createSignal("");
  const [recording, setRecording] = createSignal(false);
  const recordShortcut = (e: KeyboardEvent) => {
    if (!recording() || e.key === "Tab") return;
    e.preventDefault();
    if (e.key === "Escape") return setRecording(false);
    const next = shortcutFromKey(e);
    if (!next) return;
    setRecording(false);
    if (next !== settings().quickAskShortcut) void saveSetting({ quickAskShortcut: next }, setQuickError);
  };

  const appVersion = createFetch(() => null, getVersion);

  const [menuBar, setMenuBar] = createSignal(true);
  const [menuBarError, setMenuBarError] = createSignal("");
  api.menuBarVisible().then(setMenuBar, (e) => setMenuBarError(errorText(e)));
  const changeMenuBar = async (on: boolean) => {
    setMenuBar(on);
    setMenuBarError("");
    try {
      await api.setMenuBarVisible(on);
    } catch (e) {
      setMenuBar(!on);
      setMenuBarError(errorText(e));
    }
  };

  return (
    <Page title="General">
      <div class="settings">
        <div class="section-title settings-caption">
          <h2>Accounts</h2>
          <button
            type="button"
            class={["icon-button", { spinning: checking() }]}
            aria-label="Check again"
            title="Check again"
            aria-busy={checking() ? "true" : "false"}
            disabled={checking()}
            onClick={checkAccounts}
          >
            <RefreshIcon />
          </button>
        </div>
        <div class="list">
          <For each={PROVIDERS}>
            {(provider) => {
              const status = () => statuses()?.find((s) => s.provider === provider);
              return (
                <div class="list-row settings-row">
                  <span class="status-dot" data-state={accountState(status())} aria-hidden="true" />
                  <div class="list-main">
                    <div class="list-title">{PROVIDER_NAMES[provider]}</div>
                    <div class="list-sub">{accountSummary(status())}</div>
                    <Show when={status()?.detail}>{(detail) => <p class="settings-detail"><WithCode text={detail()} /></p>}</Show>
                  </div>
                  <Show when={status()?.version}>{(version) => <span class="settings-value">{version()}</span>}</Show>
                </div>
              );
            }}
          </For>
        </div>
        <Show when={statusError()}>
          <div class="error-box settings-error">{statusError()}</div>
        </Show>

        <h2 class="section-title">Claude billing</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Pay with</div>
            </div>
            <Segmented
              label="Pay for Claude with"
              value={billing()}
              options={[
                { value: "subscription", label: "Claude subscription" },
                { value: "api", label: "API key" },
              ]}
              onChange={changeBilling}
            />
          </div>
        </div>
        <p class="hint settings-footnote">
          <Show
            when={billing() === "api"}
            fallback="Recommended. Claude uses your claude.ai Pro or Max sign-in, and Grimoire hides any Anthropic API key from it."
          >
            Claude uses the Anthropic API key in your environment, and that key is billed.
          </Show>
          <br />
          Changing this restarts idle Claude familiars.
        </p>
        <Show when={billingError()}>
          <div class="error-box settings-error">{billingError()}</div>
        </Show>

        <h2 class="section-title">Dock</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Show familiars on the screen edge</div>
            </div>
            <Segmented
              label="Dock position"
              value={settings().dockEdge}
              options={[
                { value: "off", label: "Off" },
                { value: "left", label: "Left" },
                { value: "right", label: "Right" },
              ]}
              onChange={changeDock}
            />
          </div>
        </div>
        <Show when={settings().dockEdge !== "off" && props.agents.length}>
          <div class="list dock-picks" role="group" aria-label="Familiars in the dock">
            <For each={props.agents}>
              {(agent) => (
                <div class="list-row settings-row">
                  <div class="list-main dock-pick">
                    <BotAvatar avatar={agent.avatar} size={24} alive={false} />
                    <div class="list-title">{agent.name}</div>
                  </div>
                  <Toggle label={`Show ${agent.name} in the dock`} checked={!dockHidden().includes(agent.id)} onChange={(on) => showInDock(agent.id, on)} />
                </div>
              )}
            </For>
          </div>
        </Show>
        <p class="hint settings-footnote">
          Click a familiar in the dock to chat without opening Grimoire. Turn off the ones you don't want there; new familiars show up until you do. While the dock is on, closing the Grimoire window keeps your familiars running.
        </p>
        <Show when={dockError()}>
          <div class="error-box settings-error">{dockError()}</div>
        </Show>

        <h2 class="section-title">Quick Ask</h2>
        <div class="list">
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Open Quick Ask from any app</div>
              <p class="settings-detail">Ask a familiar something or answer what it's waiting on, without switching to Grimoire.</p>
            </div>
            <Toggle
              label="Open Quick Ask from any app"
              checked={settings().quickAskEnabled}
              onChange={(on) => void saveSetting({ quickAskEnabled: on }, setQuickError)}
            />
          </div>
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Shortcut</div>
            </div>
            <button
              type="button"
              class={["ghost small quick-shortcut", { recording: recording() }]}
              aria-label={recording() ? "Press the new shortcut, or Escape to cancel" : `Shortcut ${shortcutLabel(settings().quickAskShortcut)}. Click to change`}
              onClick={() => setRecording(true)}
              onKeyDown={recordShortcut}
              onBlur={() => setRecording(false)}
            >
              {recording() ? "Press keys" : shortcutLabel(settings().quickAskShortcut)}
            </button>
          </div>
        </div>
        <p class="hint settings-footnote">
          Use Command, Control, or Option with another key. Grimoire needs no Accessibility or Input Monitoring permission for this. Shortcuts macOS keeps for itself, like ⌘Space for Spotlight, never reach Grimoire.
        </p>
        <Show when={quickError() || settings().quickAskError}>
          {(message) => <div class="error-box settings-error">{message()}</div>}
        </Show>

        <h2 class="section-title">Menu bar</h2>
        <div class="list">
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Show Grimoire in the menu bar</div>
              <p class="settings-detail">Lists your familiars and how many are working. Click one to open its chat.</p>
            </div>
            <Toggle label="Show Grimoire in the menu bar" checked={menuBar()} onChange={(on) => void changeMenuBar(on)} />
          </div>
        </div>
        <Show when={menuBarError()}>
          <div class="error-box settings-error">{menuBarError()}</div>
        </Show>

        <BackgroundSection />

        <BackupSection />

        <h2 class="section-title">About</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Grimoire</div>
              <p class="settings-detail">
                Grimoire keeps its data on this Mac in <code>~/Library/Application Support/dev.talix.grimoire</code>.
              </p>
            </div>
            <span class="settings-value">Version {appVersion() ?? "0.5.0"}</span>
          </div>
        </div>
      </div>
    </Page>
  );
}

function AppearanceSettings() {
  const [mode, setModeSignal] = createSignal<ModeSetting>(readMode());
  const changeMode = (next: ModeSetting) => {
    setModeSignal(next);
    setMode(next);
  };
  const agents = createFetch(() => null, api.listAgents);

  return (
    <Page title="Appearance">
      <div class="settings">
        <h2 class="section-title">Theme</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Mode</div>
            </div>
            <Segmented
              label="Mode"
              value={mode()}
              options={[
                { value: "system", label: "System" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
              onChange={changeMode}
            />
          </div>
        </div>
        <ThemePicker mode={mode()} agents={agents() ?? []} />

        <h2 class="section-title">App icon</h2>
        <div class="icon-choices" role="radiogroup" aria-label="App icon">
          <For each={APP_ICONS}>
            {(option) => (
              <button
                type="button"
                role="radio"
                aria-checked={appIcon() === option.value ? "true" : "false"}
                class={["icon-choice", { selected: appIcon() === option.value }]}
                onClick={() => setAppIcon(option.value)}
              >
                <span class="icon-choice-art">
                  <For each={option.art}>{(src) => <img src={src} alt="" />}</For>
                </span>
                <span>{option.label}</span>
              </button>
            )}
          </For>
        </div>
        <p class="hint settings-footnote">The Dock shows your choice while Grimoire is open. Finder and a closed Dock icon keep the paper one.</p>
      </div>
    </Page>
  );
}

export type SettingsSection = "general" | "appearance" | "notifications" | "vault" | "connectors" | "permissions" | "links" | "phone" | "usage" | "updates" | "channels";

/** The sections, with words the ⌘K palette also finds them by. */
export const SETTINGS_SECTIONS: { id: SettingsSection; label: string; icon: () => JSX.Element; keywords: string }[] = [
  { id: "general", label: "General", icon: GearIcon, keywords: "claude billing dock providers about menu bar quick ask shortcut hotkey background login awake sleep" },
  { id: "appearance", label: "Appearance", icon: PaletteIcon, keywords: "theme dark light mode app icon colors" },
  { id: "notifications", label: "Notifications", icon: BellIcon, keywords: "alerts banners finished approvals rituals" },
  { id: "vault", label: "Vault", icon: VaultIcon, keywords: "keychain passwords logins api keys cards secrets" },
  { id: "connectors", label: "Connectors", icon: PlugIcon, keywords: "mcp servers linear notion github sentry jira oauth sign in apps integrations" },
  { id: "permissions", label: "Permissions", icon: ShieldIcon, keywords: "approvals rules commands ask allow" },
  { id: "links", label: "Linked Macs", icon: LinkIcon, keywords: "link pair tailscale tailnet other mac devices" },
  { id: "phone", label: "Phone", icon: PhoneIcon, keywords: "phone iphone android mobile companion pair tailscale" },
  { id: "usage", label: "Usage", icon: BarsIcon, keywords: "tokens plan limits rate" },
  { id: "updates", label: "Updates", icon: DownloadIcon, keywords: "version release download" },
  { id: "channels", label: "Channels", icon: ChannelIcon, keywords: "telegram bot botfather chat messages away alerts" },
];

export default function SettingsPage(props: { section?: SettingsSection; agents: Agent[] }) {
  const [section, setSection] = createSignal<SettingsSection>(props.section ?? "general");
  // Follow requests to jump to a section, like the sidebar's update notice.
  createEffect(
    () => props.section,
    (next) => {
      if (next) setSection(next);
    },
  );
  const appVersion = createFetch(() => null, getVersion);
  return (
    <div class="settings-shell">
      <nav class="settings-nav" aria-label="Settings sections" data-tauri-drag-region>
        <For each={SETTINGS_SECTIONS}>
          {(item) => (
            <button
              type="button"
              class={["nav-item", { active: section() === item.id }]}
              aria-current={section() === item.id ? "page" : undefined}
              onClick={() => setSection(item.id)}
            >
              <item.icon />
              <span>{item.label}</span>
            </button>
          )}
        </For>
      </nav>
      <Switch>
        <Match when={section() === "general"}>
          <GeneralSettings agents={props.agents} />
        </Match>
        <Match when={section() === "appearance"}>
          <AppearanceSettings />
        </Match>
        <Match when={section() === "notifications"}>
          <NotificationsPage />
        </Match>
        <Match when={section() === "vault"}>
          <VaultPage agents={props.agents} />
        </Match>
        <Match when={section() === "connectors"}>
          <ConnectorsPage agents={props.agents} />
        </Match>
        <Match when={section() === "permissions"}>
          <PermissionsPage onOpenVault={() => setSection("vault")} />
        </Match>
        <Match when={section() === "links"}>
          <LinkedMacsPage />
        </Match>
        <Match when={section() === "phone"}>
          <PhonePage />
        </Match>
        <Match when={section() === "usage"}>
          <UsagePage agents={props.agents} />
        </Match>
        <Match when={section() === "updates"}>
          <UpdatesPage version={appVersion() ?? "0.5.0"} />
        </Match>
        <Match when={section() === "channels"}>
          <ChannelsPage />
        </Match>
      </Switch>
    </div>
  );
}

/** Settings over the app, closed with the X, Escape, or a click outside. */
export function SettingsDialog(props: { section: SettingsSection; agents: Agent[]; onClose: () => void }) {
  const onKey = (e: KeyboardEvent) => {
    // A sheet opened from Settings (a vault item, the theme editor) takes Escape first.
    if (e.key === "Escape" && !document.querySelector(".sheet-backdrop")) props.onClose();
  };
  onSettled(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
  return (
    <div class="settings-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="settings-dialog" role="dialog" aria-modal="true" aria-label="Settings">
        <button type="button" class="icon-button settings-close" aria-label="Close settings" title="Close (Esc)" onClick={props.onClose}>
          <CloseIcon />
        </button>
        <SettingsPage agents={props.agents} section={props.section} />
      </div>
    </div>
  );
}
