import { createSignal, For, onSettled, Show } from "solid-js";
import { api, errorText, PROVIDER_NAMES, type Provider, type ProviderStatus } from "./api";
import BotAvatar from "./BotAvatar";
import { accountState, accountSummary, WithCode } from "./SettingsPage";
import { defaultProvider, ready, starterDraft, STARTERS, type Starter } from "./starters";
import { Segmented } from "./ui";
import "./settings.css";
import "./onboarding.css";

const PROVIDERS: Provider[] = ["codex", "claude"];

const INSTALL_NOTE: Record<Provider, string> = {
  codex: "Installs with Homebrew, or npm when Homebrew isn't there.",
  claude: "Installs with Anthropic's installer. Nothing else is needed.",
};

/**
 * The first-run wizard: get Codex or Claude installed and signed in, then summon a first familiar
 * from a starter. Skipping it opens the usual summon form.
 */
export default function Onboarding(props: { onCreated: (agentId: string) => void; onSkip: () => void }) {
  const [step, setStep] = createSignal<"tools" | "starter">("tools");
  const [statuses, setStatuses] = createSignal<ProviderStatus[]>([]);
  const [checking, setChecking] = createSignal(true);
  const [installing, setInstalling] = createSignal<Provider | null>(null);
  const [error, setError] = createSignal("");

  const check = async () => {
    setChecking(true);
    try {
      setStatuses(await api.providerStatus());
    } catch (e) {
      setError(errorText(e));
    } finally {
      setChecking(false);
    }
  };
  onSettled(() => void check());

  const statusOf = (p: Provider) => statuses().find((s) => s.provider === p);
  const anyReady = () => PROVIDERS.some((p) => ready(statusOf(p)));

  const install = async (provider: Provider) => {
    setInstalling(provider);
    setError("");
    try {
      await api.installProvider(provider);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setInstalling(null);
    }
    await check();
  };

  const signIn = (provider: Provider) => {
    setError("");
    api.openSignIn(provider).catch((e) => setError(errorText(e)));
  };

  const skip = async () => {
    setError("");
    try {
      await api.finishOnboarding();
      props.onSkip();
    } catch (e) {
      setError(errorText(e));
    }
  };

  return (
    <div class="onboarding" data-tauri-drag-region>
      <div class="onboarding-card">
        <p class="onboarding-step">Step {step() === "tools" ? 1 : 2} of 2</p>
        <Show
          when={step() === "starter"}
          fallback={
            <>
              <h1>Welcome to Grimoire</h1>
              <p class="hint">
                Familiars run on the Codex or Claude command-line tools. Install and sign in to at least one. Grimoire checks for both and can install either.
              </p>
              <div class="list">
                <For each={PROVIDERS}>
                  {(provider) => {
                    const status = () => (checking() && !statusOf(provider) ? undefined : statusOf(provider));
                    return (
                      <div class="list-row settings-row onboarding-tool">
                        <span class="status-dot" data-state={accountState(status())} aria-hidden="true" />
                        <div class="list-main">
                          <div class="list-title">{PROVIDER_NAMES[provider]}</div>
                          <div class="list-sub">
                            {accountSummary(status())}
                            <Show when={status()?.version}>{(v) => <> · {v()}</>}</Show>
                          </div>
                          <Show when={status() && !status()!.installed}>
                            <p class="settings-detail">{INSTALL_NOTE[provider]}</p>
                          </Show>
                          <Show when={status()?.installed && status()?.signedIn === false}>
                            <p class="settings-detail">Sign in opens Terminal, which opens your browser. Come back here when it says you're signed in.</p>
                          </Show>
                          <Show when={status()?.installed && status()?.signedIn === null && status()?.detail}>
                            {(detail) => (
                              <p class="settings-detail">
                                <WithCode text={detail()} />
                              </p>
                            )}
                          </Show>
                        </div>
                        <Show when={status() && !status()!.installed}>
                          <button type="button" class="ghost small" disabled={installing() !== null} onClick={() => void install(provider)}>
                            {installing() === provider ? "Installing..." : "Install"}
                          </button>
                        </Show>
                        <Show when={status()?.installed && !ready(status())}>
                          <button type="button" class="ghost small" onClick={() => signIn(provider)}>
                            Sign in
                          </button>
                        </Show>
                      </div>
                    );
                  }}
                </For>
              </div>
              <Show when={error()}>
                <p class="error-box onboarding-error" role="alert">
                  {error()}
                </p>
              </Show>
              <div class="onboarding-actions">
                <button type="button" class="ghost" onClick={() => void skip()}>
                  Skip setup
                </button>
                <span class="onboarding-spacer" />
                <button type="button" class="ghost" disabled={checking() || installing() !== null} onClick={() => void check()}>
                  {checking() ? "Checking..." : "Check again"}
                </button>
                <button type="button" class="primary" disabled={!anyReady()} onClick={() => setStep("starter")}>
                  Continue
                </button>
              </div>
            </>
          }
        >
          <StarterStep statuses={statuses()} onBack={() => setStep("tools")} onCreated={props.onCreated} />
        </Show>
      </div>
    </div>
  );
}

function StarterStep(props: { statuses: ProviderStatus[]; onBack: () => void; onCreated: (agentId: string) => void }) {
  const [starter, setStarter] = createSignal<Starter>(STARTERS[0]);
  const [name, setName] = createSignal(STARTERS[0].name);
  const [provider, setProvider] = createSignal<Provider>(defaultProvider(props.statuses) ?? "codex");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const readyProviders = () => PROVIDERS.filter((p) => ready(props.statuses.find((s) => s.provider === p)));

  const pick = (next: Starter) => {
    // Keep a name the person typed; follow the starter otherwise.
    if (name() === starter().name) setName(next.name);
    setStarter(next);
  };

  const create = async (e: SubmitEvent) => {
    e.preventDefault();
    if (busy()) return;
    setBusy(true);
    setError("");
    try {
      const id = await api.importTemplate(starterDraft(starter(), name(), provider()));
      // The familiar now exists, so the wizard won't open again even if this save fails.
      await api.finishOnboarding().catch(() => {});
      props.onCreated(id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="onboarding-starter" onSubmit={create}>
      <h1>Summon your first familiar</h1>
      <p class="hint">Pick a starter. It works only in its own folder under ~/Grimoire, and you can change its rules in Memory later.</p>
      <div class="starter-grid" role="radiogroup" aria-label="Starter familiars">
        <For each={STARTERS}>
          {(s) => (
            <button
              type="button"
              role="radio"
              aria-checked={starter().id === s.id ? "true" : "false"}
              class={["starter-card", { active: starter().id === s.id }]}
              style={{ "--agent": s.avatar.color }}
              onClick={() => pick(s)}
            >
              <BotAvatar avatar={s.avatar} size={40} alive={starter().id === s.id} />
              <span class="starter-name">{s.name}</span>
              <span class="starter-blurb">{s.blurb}</span>
            </button>
          )}
        </For>
      </div>
      <div class="form-grid">
        <label class="field">
          <span>Name</span>
          <input value={name()} onInput={(e) => setName(e.currentTarget.value)} required maxlength={60} />
        </label>
        <div class="field">
          <span>Runs on</span>
          <Show when={readyProviders().length > 1} fallback={<p class="readonly">{PROVIDER_NAMES[provider()]}</p>}>
            <Segmented label="Runs on" value={provider()} options={readyProviders().map((p) => ({ value: p, label: PROVIDER_NAMES[p] }))} onChange={setProvider} />
          </Show>
        </div>
      </div>
      <Show when={error()}>
        <p class="error-box onboarding-error" role="alert">
          {error()}
        </p>
      </Show>
      <div class="onboarding-actions">
        <button type="button" class="ghost" disabled={busy()} onClick={props.onBack}>
          Back
        </button>
        <span class="onboarding-spacer" />
        <button type="submit" class="primary" disabled={busy() || !name().trim()}>
          {busy() ? "Summoning..." : `Summon ${name().trim() || starter().name}`}
        </button>
      </div>
    </form>
  );
}
