import { openUrl } from "@tauri-apps/plugin-opener";
import { createSignal, onCleanup, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type TelegramCode, type TelegramStatus } from "./api";
import { countdown } from "./linked";
import { Page, Toggle } from "./ui";

/** Settings > Channels: a Telegram bot that reaches this Mac's familiars. */
export default function ChannelsPage() {
  const [status, setStatus] = createSignal<TelegramStatus | null>(null);
  const [error, setError] = createSignal("");
  const load = async () => {
    try {
      setStatus(await api.telegramStatus());
    } catch (e) {
      setError(errorText(e));
    }
  };
  onMountAsync(async (keep) => {
    keep(await onEvent("telegram_changed", () => void load()));
    await load();
  });

  const [now, setNow] = createSignal(Date.now());
  const tick = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(tick));

  /** Runs a settings change and shows its result or error. */
  const act = async (change: () => Promise<TelegramStatus | void>) => {
    setError("");
    try {
      const next = await change();
      if (next) setStatus(next);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const [token, setToken] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const saveToken = async (e: SubmitEvent) => {
    e.preventDefault();
    setSaving(true);
    await act(async () => {
      const next = await api.telegramSetToken(token());
      setToken("");
      return next;
    });
    setSaving(false);
  };

  const [pairing, setPairing] = createSignal<TelegramCode | null>(null);
  const left = () => {
    const code = pairing();
    return code ? countdown(code.expiresAt, now()) : null;
  };
  const showCode = () =>
    act(async () => {
      setPairing(await api.telegramStartPairing());
      setNow(Date.now());
    });

  const [confirming, setConfirming] = createSignal<"unpair" | "token" | null>(null);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));
  /** Runs `change` on the second click within 4 seconds. */
  const confirmThen = (what: "unpair" | "token", change: () => Promise<TelegramStatus>) => {
    if (confirming() !== what) {
      setConfirming(what);
      clearTimeout(confirmTimer);
      confirmTimer = window.setTimeout(() => setConfirming(null), 4000);
      return;
    }
    setConfirming(null);
    setPairing(null);
    void act(change);
  };

  const owner = () => status()?.owner ?? null;

  return (
    <Page title="Channels">
      <div class="settings">
        <h2 class="section-title">Telegram</h2>
        <div class="list">
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Use Telegram</div>
              <p class="settings-detail">
                Chat with this Mac's familiars from a Telegram bot you make yourself, and get their questions, drafts, and alerts there. It needs no
                Tailscale and no open port.
              </p>
            </div>
            <Toggle label="Use Telegram" checked={status()?.enabled ?? false} onChange={(on) => void act(() => api.telegramSetEnabled(on))} />
          </div>

          <Show
            when={status()?.hasToken}
            fallback={
              <form class="list-row link-form" onSubmit={saveToken}>
                <label class="field">
                  <span>Bot token</span>
                  <input
                    type="password"
                    value={token()}
                    onInput={(e) => setToken(e.currentTarget.value)}
                    placeholder="123456789:AAH…"
                    autocomplete="off"
                    spellcheck={false}
                  />
                </label>
                <button type="submit" class="ghost small" disabled={saving() || !token().trim()}>
                  {saving() ? "Checking…" : "Save token"}
                </button>
              </form>
            }
          >
            <div class="list-row settings-row">
              <div class="list-main">
                <div class="list-title">Bot</div>
                <div class="list-sub">{status()?.bot ? `@${status()!.bot}` : "Token saved"}</div>
              </div>
              <button type="button" class="ghost small danger" onClick={() => confirmThen("token", api.telegramForgetToken)}>
                {confirming() === "token" ? "Click again to remove" : "Remove token"}
              </button>
            </div>
          </Show>

          <Show when={status()?.hasToken && status()?.enabled}>
            <Show
              when={owner()}
              fallback={
                <div class="list-row settings-row">
                  <div class="list-main">
                    <div class="list-title">Pairing code</div>
                    <Show
                      when={pairing() && left()}
                      fallback={
                        <p class="settings-detail">
                          {pairing() ? "That code expired. Show a new one to pair." : "Show a code, then send it to your bot to pair your Telegram account."}
                        </p>
                      }
                    >
                      <p class="pairing-code" aria-label={`Pairing code ${pairing()!.code}`}>
                        {pairing()!.code}
                      </p>
                      <p class="settings-detail" role="timer">
                        Expires in {left()}
                      </p>
                      <p class="settings-detail">
                        Send <code>/start {pairing()!.code}</code> to {status()?.bot ? `@${status()!.bot}` : "your bot"}
                        <Show when={pairing()!.link}>
                          {(link) => (
                            <>
                              , or{" "}
                              <a href={link()} onClick={(e) => (e.preventDefault(), void openUrl(link()))}>
                                open it in Telegram
                              </a>
                            </>
                          )}
                        </Show>
                        .
                      </p>
                    </Show>
                  </div>
                  <button type="button" class="ghost small" onClick={() => void showCode()}>
                    {pairing() && left() ? "New code" : "Show a pairing code"}
                  </button>
                </div>
              }
            >
              {(paired) => (
                <div class="list-row settings-row">
                  <div class="list-main">
                    <div class="list-title">Paired with {paired().name}</div>
                    <div class="list-sub">{status()?.current ? `Messages go to ${status()!.current}` : "Send /familiars in the chat to pick a familiar"}</div>
                  </div>
                  <button type="button" class="ghost small danger" onClick={() => confirmThen("unpair", api.telegramUnpair)}>
                    {confirming() === "unpair" ? "Click again to unpair" : "Unpair"}
                  </button>
                </div>
              )}
            </Show>
          </Show>
        </div>
        <Show when={error() || status()?.error}>
          {(text) => (
            <p class="error-box settings-error" role="alert">
              {text()}
            </p>
          )}
        </Show>
        <p class="hint settings-footnote">
          To make a bot, message @BotFather in Telegram, send /newbot, and paste the token it gives you here. The token stays in your Keychain.
          Telegram's servers can read everything in bot chats, unlike the phone companion, which stays on your tailnet. Only the account you pair
          is heard. Approvals show up in the chat, but you answer them on this Mac.
        </p>
      </div>
    </Page>
  );
}
