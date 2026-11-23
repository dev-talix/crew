import { getVersion } from "@tauri-apps/api/app";
import { createSignal, For, Match, onSettled, Show, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import { api, createFetch, errorText, PROVIDER_NAMES, type Agent, type DockEdge, type Provider, type ProviderStatus, type Settings } from "./api";
import { Page, Segmented } from "./ui";
import { ThemePicker } from "./ThemeEditor";
import { appIcon, readMode, setAppIcon, setMode, type AppIconSetting, type ModeSetting } from "./themeStore";
import nightIcon from "./assets/icon-night.svg";
import paperIcon from "./assets/icon-paper.svg";
import { GearIcon, PaletteIcon, VaultIcon } from "./icons";
import VaultPage from "./VaultPage";
import "./settings.css";

type Billing = Settings["claudeBilling"];

const PROVIDERS: Provider[] = ["codex", "claude"];
const APP_ICONS: { value: AppIconSetting; label: string; art: string[] }[] = [
  { value: "auto", label: "Match appearance", art: [paperIcon, nightIcon] },
  { value: "paper", label: "Paper", art: [paperIcon] },
  { value: "night", label: "Night", art: [nightIcon] },
];

function accountState(status: ProviderStatus | undefined): "loading" | "ok" | "unknown" | "off" {
  if (!status) return "loading";
  if (!status.installed || status.signedIn === false) return "off";
  return status.signedIn ? "ok" : "unknown";
}

function accountSummary(status: ProviderStatus | undefined): string {
  if (!status) return "Checking…";
  if (!status.installed) return "Not installed";
  if (status.signedIn === false) return "Not signed in";
  if (status.signedIn) return status.method ? `Signed in with ${status.method}` : "Signed in";
  return "Sign-in status unknown";
}

// Renders `backticked` spans as inline code.
function WithCode(props: { text: string }) {
  return <For each={props.text.split("`")}>{(part, i) => (i() % 2 ? <code>{part}</code> : part)}</For>;
}

function RefreshIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9M13.5 2.5v3h-3" />
    </svg>
  );
}

function GeneralSettings() {
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

  const [settings, setSettings] = createSignal<Settings>({ claudeBilling: "subscription", dockEdge: "off" });
  const [billingError, setBillingError] = createSignal("");
  const [dockError, setDockError] = createSignal("");
  api.getSettings().then(setSettings, (e) => setBillingError(errorText(e)));
  const billing = () => settings().claudeBilling;

  const saveSetting = async (patch: Partial<Settings>, setError: (message: string) => void) => {
    const previous = settings();
    const next = { ...previous, ...patch };
    setSettings(next);
    setError("");
    try {
      setSettings(await api.saveSettings(next));
    } catch (e) {
      setSettings(previous);
      setError(errorText(e));
    }
  };

  const changeBilling = (next: Billing) => {
    if (next !== billing()) void saveSetting({ claudeBilling: next }, setBillingError);
  };

  const changeDock = (next: DockEdge) => {
    if (next !== settings().dockEdge) void saveSetting({ dockEdge: next }, setDockError);
  };

  const appVersion = createFetch(() => null, getVersion);

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
        <p class="hint settings-footnote">
          Click a familiar in the dock to chat without opening Grimoire. While the dock is on, closing the Grimoire window keeps your familiars running.
        </p>
        <Show when={dockError()}>
          <div class="error-box settings-error">{dockError()}</div>
        </Show>

        <h2 class="section-title">About</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Grimoire</div>
              <p class="settings-detail">
                Grimoire keeps its data on this Mac in <code>~/Library/Application Support/dev.talix.grimoire</code>.
              </p>
            </div>
            <span class="settings-value">Version {appVersion() ?? "0.1.0"}</span>
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

export type SettingsSection = "general" | "appearance" | "vault";

const SECTIONS: { id: SettingsSection; label: string; icon: () => JSX.Element }[] = [
  { id: "general", label: "General", icon: GearIcon },
  { id: "appearance", label: "Appearance", icon: PaletteIcon },
  { id: "vault", label: "Vault", icon: VaultIcon },
];

export default function SettingsPage(props: { section?: SettingsSection; agents: Agent[] }) {
  const [section, setSection] = createSignal<SettingsSection>(props.section ?? "general");
  return (
    <div class="settings-shell">
      <nav class="settings-nav" aria-label="Settings sections" data-tauri-drag-region>
        <For each={SECTIONS}>
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
          <GeneralSettings />
        </Match>
        <Match when={section() === "appearance"}>
          <AppearanceSettings />
        </Match>
        <Match when={section() === "vault"}>
          <VaultPage agents={props.agents} />
        </Match>
      </Switch>
    </div>
  );
}
