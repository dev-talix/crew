import { createSignal, For, onSettled, Show } from "solid-js";
import { api, errorText, type Delegation } from "./api";
import { Toggle } from "./ui";

const SCOPES: { value: Delegation["scope"]; label: string; about: string }[] = [
  { value: "anyone", label: "Any familiar", about: "Any familiar can ask any other one for help and wait for its reply." },
  { value: "coven", label: "Same coven only", about: "A familiar can only ask familiars in its own coven. Unbound familiars can't ask anyone." },
  { value: "nobody", label: "Nobody", about: "Familiars can't ask each other." },
];

/** The Familiars section of Settings, Permissions: who can ask whom with ask_familiar. */
export default function DelegationSettings() {
  const [settings, setSettings] = createSignal<Delegation | null>(null);
  const [error, setError] = createSignal("");
  onSettled(() => {
    api.getDelegation().then(setSettings, (e) => setError(errorText(e)));
  });

  const save = async (next: Delegation) => {
    const previous = settings();
    setSettings(next);
    setError("");
    try {
      setSettings(await api.saveDelegation(next));
    } catch (e) {
      setSettings(previous);
      setError(errorText(e));
    }
  };

  return (
    <>
      <h2 class="section-title">Familiars asking familiars</h2>
      <Show when={settings()}>
        {(s) => (
          <div class="list">
            <div class="list-row settings-row">
              <div class="list-main">
                <div class="list-title">Who can ask whom</div>
                <p class="settings-detail">{SCOPES.find((o) => o.value === s().scope)?.about}</p>
              </div>
              <select
                class="permissions-mode"
                aria-label="Who can ask whom"
                value={s().scope}
                onChange={(e) => void save({ ...s(), scope: e.currentTarget.value as Delegation["scope"] })}
              >
                <For each={SCOPES}>{(o) => <option value={o.value}>{o.label}</option>}</For>
              </select>
            </div>
            <div class="list-row settings-row">
              <div class="list-main">
                <div class="list-title">Approve each question</div>
                <p class="settings-detail">Grimoire shows you the message first, like "Researcher wants to ask Scribe", and sends it only if you allow it.</p>
              </div>
              <Toggle
                label="Approve each question between familiars"
                checked={s().approve}
                disabled={s().scope === "nobody"}
                onChange={(approve) => void save({ ...s(), approve })}
              />
            </div>
          </div>
        )}
      </Show>
      <p class="hint settings-footnote">
        A familiar waits up to 10 minutes for a reply. It can't ask itself or a familiar that is waiting on it, and chains stop at 3 familiars in a row.
      </p>
      <Show when={error()}>
        <p class="error-box settings-error" role="alert">
          {error()}
        </p>
      </Show>
    </>
  );
}
