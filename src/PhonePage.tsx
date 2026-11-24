import { createSignal, For, onCleanup, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, relativeTime, type CompanionStatus, type PairedPhone, type PhonePairingCode } from "./api";
import { countdown } from "./linked";
import { Page } from "./ui";

/** Settings > Phone: pair a phone with the companion web app and see the phones paired with this Mac. */
export default function PhonePage() {
  const [status, setStatus] = createSignal<CompanionStatus | null>(null);
  const [loadError, setLoadError] = createSignal("");
  const load = async () => {
    try {
      setStatus(await api.companionStatus());
      setLoadError("");
    } catch (e) {
      setLoadError(errorText(e));
    }
  };
  onMountAsync(async (keep) => {
    keep(await onEvent("companion_changed", () => void load()));
    await load();
  });

  const [now, setNow] = createSignal(Date.now());
  const tick = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(tick));

  const [pairing, setPairing] = createSignal<PhonePairingCode | null>(null);
  const [pairingError, setPairingError] = createSignal("");
  const [starting, setStarting] = createSignal(false);
  const left = () => {
    const code = pairing();
    return code ? countdown(code.expiresAt, now()) : null;
  };
  const showCode = async () => {
    setStarting(true);
    setPairingError("");
    try {
      setPairing(await api.companionStartPairing());
      setNow(Date.now());
      void load();
    } catch (e) {
      setPairingError(errorText(e));
    } finally {
      setStarting(false);
    }
  };
  // The address to open: the server's once it listens, or what the pairing call saw.
  const url = () => status()?.urls[0] ?? pairing()?.urls[0];

  const [confirming, setConfirming] = createSignal<string | null>(null);
  const [removeError, setRemoveError] = createSignal("");
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));
  const remove = async (phone: PairedPhone) => {
    if (confirming() !== phone.id) {
      setConfirming(phone.id);
      clearTimeout(confirmTimer);
      confirmTimer = window.setTimeout(() => setConfirming(null), 4000);
      return;
    }
    setConfirming(null);
    setRemoveError("");
    try {
      await api.companionRemove(phone.id);
      void load();
    } catch (e) {
      setRemoveError(errorText(e));
    }
  };

  return (
    <Page title="Phone">
      <div class="settings">
        <h2 class="section-title">Pair a phone</h2>
        <div class="list">
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Pairing code</div>
              <Show
                when={pairing() && left()}
                fallback={
                  <p class="settings-detail">
                    {pairing()
                      ? "That code expired. Show a new one to pair a phone."
                      : "Answer the inbox, send drafts, and chat with familiars from your phone. Show a code here, then open the address on your phone."}
                  </p>
                }
              >
                <p class="pairing-code" aria-label={`Pairing code ${pairing()!.code}`}>
                  {pairing()!.code}
                </p>
                <p class="settings-detail" role="timer">
                  Expires in {left()}
                </p>
                <Show when={url()} fallback={<p class="settings-detail">Waiting for Tailscale on this Mac…</p>}>
                  <p class="settings-detail">
                    On your phone, open <code>{url()}</code> and enter this code.
                  </p>
                </Show>
              </Show>
            </div>
            <button type="button" class="ghost small" disabled={starting()} onClick={() => void showCode()}>
              {pairing() && left() ? "New code" : "Show a pairing code"}
            </button>
          </div>
        </div>
        <Show when={pairingError() || status()?.error || loadError()}>
          {(error) => (
            <p class="error-box settings-error" role="alert">
              {error()}
            </p>
          )}
        </Show>

        <h2 class="section-title">Paired phones</h2>
        <div class="list">
          <For
            each={status()?.phones ?? []}
            fallback={
              <div class="list-row">
                <span class="list-main list-sub">No phones yet.</span>
              </div>
            }
          >
            {(phone) => (
              <div class="list-row settings-row">
                <div class="list-main">
                  <div class="list-title">{phone.name}</div>
                  <div class="list-sub">{phone.lastSeen ? `Last seen ${relativeTime(phone.lastSeen, now())}` : "Not seen yet"}</div>
                </div>
                <button type="button" class="ghost small danger" onClick={() => void remove(phone)}>
                  {confirming() === phone.id ? "Click again to remove" : "Remove"}
                </button>
              </div>
            )}
          </For>
        </div>
        <Show when={removeError()}>
          <p class="error-box settings-error" role="alert">
            {removeError()}
          </p>
        </Show>
        <p class="hint settings-footnote">
          Your phone needs Tailscale, signed in to the same tailnet as this Mac. Grimoire only listens for phones while one is paired or a code is
          showing. From a phone you can allow an approval once or deny it. Always allow stays on this Mac.
        </p>
      </div>
    </Page>
  );
}
