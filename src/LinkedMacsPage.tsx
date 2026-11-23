import { createSignal, For, onCleanup, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type LinkStatus, type PairingCode, type Peer } from "./api";
import { countdown, peerState } from "./linked";
import { RefreshIcon } from "./icons";
import { Page } from "./ui";

/** Settings > Linked Macs: pair with another Mac on the tailnet and see the Macs linked to this one. */
export default function LinkedMacsPage() {
  const [status, setStatus] = createSignal<LinkStatus | null>(null);
  const [loadError, setLoadError] = createSignal("");
  const [checking, setChecking] = createSignal(false);
  const load = async () => {
    setChecking(true);
    try {
      setStatus(await api.linkStatus());
      setLoadError("");
    } catch (e) {
      setLoadError(errorText(e));
    } finally {
      setChecking(false);
    }
  };
  onMountAsync(async (keep) => {
    keep(await onEvent("links_changed", () => void load()));
    await load();
  });

  const [now, setNow] = createSignal(Date.now());
  const tick = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(tick));

  const [pairing, setPairing] = createSignal<PairingCode | null>(null);
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
      setPairing(await api.linkStartPairing());
      setNow(Date.now());
    } catch (e) {
      setPairingError(errorText(e));
    } finally {
      setStarting(false);
    }
  };
  // The name the other Mac types: its tailnet name, or this Mac's name until one is known.
  const tailnetName = (code: PairingCode) => code.addresses[0] ?? code.name;

  const [address, setAddress] = createSignal("");
  const [code, setCode] = createSignal("");
  const [linking, setLinking] = createSignal(false);
  const [linkError, setLinkError] = createSignal("");
  const [linked, setLinked] = createSignal<Peer | null>(null);
  const link = async (e: SubmitEvent) => {
    e.preventDefault();
    setLinkError("");
    setLinked(null);
    if (!address().trim() || !code().trim()) {
      setLinkError("Enter the other Mac's tailnet name and the code it shows.");
      return;
    }
    setLinking(true);
    try {
      setLinked(await api.linkPair(address().trim(), code().trim()));
      setAddress("");
      setCode("");
      void load();
    } catch (err) {
      setLinkError(errorText(err));
    } finally {
      setLinking(false);
    }
  };

  const [confirming, setConfirming] = createSignal<string | null>(null);
  const [unlinkError, setUnlinkError] = createSignal("");
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));
  const unlink = async (peer: Peer) => {
    if (confirming() !== peer.id) {
      setConfirming(peer.id);
      clearTimeout(confirmTimer);
      confirmTimer = window.setTimeout(() => setConfirming(null), 4000);
      return;
    }
    setConfirming(null);
    setUnlinkError("");
    try {
      await api.linkUnlink(peer.id);
      void load();
    } catch (e) {
      setUnlinkError(errorText(e));
    }
  };

  return (
    <Page title="Linked Macs">
      <div class="settings">
        <div class="section-title settings-caption">
          <h2>This Mac</h2>
          <button
            type="button"
            class={["icon-button", { spinning: checking() }]}
            aria-label="Check again"
            title="Check again"
            aria-busy={checking() ? "true" : "false"}
            disabled={checking()}
            onClick={() => void load()}
          >
            <RefreshIcon />
          </button>
        </div>
        <div class="list">
          <Show
            when={status()?.thisMac}
            fallback={
              <div class="list-row">
                <span class="list-main list-sub">{loadError() ? "Couldn't check this Mac." : "Checking…"}</span>
              </div>
            }
          >
            {(mac) => (
              <div class="list-row settings-row">
                <span class="status-dot" data-state={mac().listening ? "ok" : "off"} aria-hidden="true" />
                <div class="list-main">
                  <div class="list-title">{mac().name}</div>
                  <div class="list-sub">{mac().listening ? "Ready to link" : "Not ready to link"}</div>
                  <Show when={mac().error}>{(error) => <p class="settings-detail">{error()}</p>}</Show>
                  <Show when={mac().addresses.length}>
                    <p class="settings-detail">
                      On the tailnet as{" "}
                      <For each={mac().addresses}>
                        {(a, i) => (
                          <>
                            {i() > 0 ? ", " : ""}
                            <code>{a}</code>
                          </>
                        )}
                      </For>
                    </p>
                  </Show>
                </div>
              </div>
            )}
          </Show>
        </div>
        <Show when={loadError()}>
          <p class="error-box settings-error" role="alert">
            {loadError()}
          </p>
        </Show>

        <h2 class="section-title">Pair with another Mac</h2>
        <div class="list">
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Pairing code</div>
              <Show
                when={pairing() && left()}
                fallback={
                  <p class="settings-detail">
                    {pairing() ? "That code expired. Show a new one to link a Mac." : "Show a code here, then enter it on the other Mac."}
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
                  On your other Mac, open Settings &gt; Linked Macs &gt; Link a Mac, and enter <code>{tailnetName(pairing()!)}</code> and this code.
                </p>
              </Show>
            </div>
            <button type="button" class="ghost small" disabled={starting() || status()?.thisMac.listening === false} onClick={() => void showCode()}>
              {pairing() && left() ? "New code" : "Show a pairing code"}
            </button>
          </div>
        </div>
        <Show when={pairingError()}>
          <p class="error-box settings-error" role="alert">
            {pairingError()}
          </p>
        </Show>

        <h2 class="section-title">Link a Mac</h2>
        <form class="list" onSubmit={link}>
          <div class="list-row link-form">
            <label class="field">
              <span>Tailnet name or address</span>
              <input
                value={address()}
                onInput={(e) => setAddress(e.currentTarget.value)}
                placeholder="studio.tail1234.ts.net"
                autocapitalize="off"
                autocorrect="off"
                spellcheck={false}
              />
            </label>
            <label class="field">
              <span>Pairing code</span>
              <input
                value={code()}
                onInput={(e) => setCode(e.currentTarget.value)}
                placeholder="The code on the other Mac"
                autocapitalize="off"
                autocorrect="off"
                spellcheck={false}
              />
            </label>
            <button type="submit" class="ghost small" disabled={linking()}>
              {linking() ? "Linking..." : "Link"}
            </button>
          </div>
        </form>
        <Show when={linked()}>
          {(peer) => (
            <p class="hint settings-footnote" role="status">
              Linked with {peer().name}. Its familiars now show up in the sidebar.
            </p>
          )}
        </Show>
        <Show when={linkError()}>
          <p class="error-box settings-error" role="alert">
            {linkError()}
          </p>
        </Show>

        <h2 class="section-title">Linked Macs</h2>
        <div class="list">
          <For
            each={status()?.peers ?? []}
            fallback={
              <div class="list-row">
                <span class="list-main list-sub">No linked Macs yet.</span>
              </div>
            }
          >
            {(peer) => (
              <div class="list-row settings-row">
                <span class="status-dot" data-state={peer.online ? "ok" : "off"} aria-hidden="true" />
                <div class="list-main">
                  <div class="list-title">{peer.name}</div>
                  <div class="list-sub">{peerState(peer, now())}</div>
                  <p class="settings-detail">
                    <code>{peer.address}</code>
                    <Show when={peer.version}>{(version) => <> · Grimoire {version()}</>}</Show>
                  </p>
                </div>
                <button type="button" class="ghost small danger" onClick={() => void unlink(peer)}>
                  {confirming() === peer.id ? "Click again to unlink" : "Unlink"}
                </button>
              </div>
            )}
          </For>
        </div>
        <Show when={unlinkError()}>
          <p class="error-box settings-error" role="alert">
            {unlinkError()}
          </p>
        </Show>
        <p class="hint settings-footnote">
          Both Macs need Tailscale running and signed in to the same tailnet. A familiar stays on the Mac it runs on and shows up on every linked
          Mac. Only its own Mac can change or delete it.
        </p>
      </div>
    </Page>
  );
}
