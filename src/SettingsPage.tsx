import { getVersion } from "@tauri-apps/api/app";
import { createSignal, For, Show } from "solid-js";
import { api, createFetch, errorText, PROVIDER_NAMES, type DockEdge, type Provider, type ProviderStatus, type Settings } from "./api";
import { Page, Segmented } from "./ui";
import "./settings.css";

type Theme = "system" | "light" | "dark";
type Billing = Settings["claudeBilling"];

const THEME_KEY = "crew-theme";
const PROVIDERS: Provider[] = ["codex", "claude"];

function readTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    // Storage can be unavailable. Fall back to the system appearance.
  }
  return "system";
}

function applyTheme(theme: Theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

export function applyStoredTheme(): void {
  applyTheme(readTheme());
}

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

export default function SettingsPage() {
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
  void checkAccounts();

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

  const [theme, setTheme] = createSignal<Theme>(readTheme());
  const changeTheme = (next: Theme) => {
    setTheme(next);
    applyTheme(next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {
      // The choice still applies for this session.
    }
  };

  const appVersion = createFetch(() => null, getVersion);

  return (
    <Page title="Settings">
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
            fallback="Recommended. Claude uses your claude.ai Pro or Max sign-in, and Crew hides any Anthropic API key from it."
          >
            Claude uses the Anthropic API key in your environment, and that key is billed.
          </Show>
          <br />
          Changing this restarts idle Claude agents.
        </p>
        <Show when={billingError()}>
          <div class="error-box settings-error">{billingError()}</div>
        </Show>

        <h2 class="section-title">Dock</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Show agents on the screen edge</div>
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
          Click an agent in the dock to chat without opening Crew. While the dock is on, closing the Crew window keeps your agents running.
        </p>
        <Show when={dockError()}>
          <div class="error-box settings-error">{dockError()}</div>
        </Show>

        <h2 class="section-title">Appearance</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Theme</div>
            </div>
            <Segmented
              label="Theme"
              value={theme()}
              options={[
                { value: "system", label: "System" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
              onChange={changeTheme}
            />
          </div>
        </div>

        <h2 class="section-title">About</h2>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Crew</div>
              <p class="settings-detail">
                Crew keeps its data on this Mac in <code>~/Library/Application Support/dev.talix.crew</code>.
              </p>
            </div>
            <span class="settings-value">Version {appVersion() ?? "0.1.0"}</span>
          </div>
        </div>
      </div>
    </Page>
  );
}
