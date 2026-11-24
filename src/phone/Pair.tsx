import { createSignal } from "solid-js";
import { api, errorText, saveToken } from "./api";

export default function Pair(props: { onPaired: () => void }) {
  const [code, setCode] = createSignal("");
  const [name, setName] = createSignal("");
  const [error, setError] = createSignal("");
  const [pairing, setPairing] = createSignal(false);
  const ready = () => code().length === 8 && name().trim().length > 0 && !pairing();

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    if (!ready()) return;
    setPairing(true);
    setError("");
    try {
      const { token } = await api.pair(code(), name().trim());
      saveToken(token);
      props.onPaired();
    } catch (err) {
      setError(errorText(err));
      setPairing(false);
    }
  };

  return (
    <div class="pair">
      <form class="pair-card" onSubmit={submit}>
        <p class="wordmark">Grimoire</p>
        <h1>Pair this phone</h1>
        <p class="pair-lead">On your Mac, open Settings, then Phone, then Show a pairing code. Type the code it shows.</p>
        <label class="field">
          <span>Code</span>
          <input
            class="code-input"
            inputmode="text"
            autocomplete="one-time-code"
            autocapitalize="characters"
            spellcheck={false}
            maxlength={8}
            placeholder="8 characters"
            value={code()}
            onInput={(e) => setCode(e.currentTarget.value.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 8))}
          />
        </label>
        <label class="field">
          <span>Name for this phone</span>
          <input maxlength={60} placeholder="My phone" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
        </label>
        {error() && (
          <p class="error-box" role="alert">
            {error()}
          </p>
        )}
        <button type="submit" class="primary wide" disabled={!ready()}>
          {pairing() ? "Pairing…" : "Pair"}
        </button>
        <p class="pair-note">Your phone needs the Tailscale app, signed in to the same tailnet as your Mac.</p>
      </form>
    </div>
  );
}
