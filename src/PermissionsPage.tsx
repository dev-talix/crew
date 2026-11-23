import { createSignal, For, onSettled, Show } from "solid-js";
import { api, errorText, type Permissions, type PermissionMode, type RuleKind } from "./api";
import DelegationSettings from "./DelegationSettings";
import { CloseIcon } from "./icons";
import { Page } from "./ui";

const MODES: { value: PermissionMode; label: string; about: string }[] = [
  { value: "off", label: "Never ask", about: "Familiars run commands on their own, within the access you gave each one." },
  { value: "rules", label: "Ask when a rule matches", about: "Grimoire checks each command against your rules and asks you when an Always ask rule matches." },
  { value: "always", label: "Ask before every command", about: "Grimoire asks you before each command, except the ones an Allow rule covers." },
];

const SECTIONS: { kind: RuleKind; title: string; about: string; example: string }[] = [
  { kind: "ask", title: "Always ask", about: "Stop and ask when a command matches one of these.", example: "git push*" },
  { kind: "allow", title: "Allow without asking", about: "Go ahead when a command matches one of these.", example: "cargo test*" },
];

export default function PermissionsPage(props: { onOpenVault: () => void }) {
  const [permissions, setPermissions] = createSignal<Permissions | null>(null);
  const [error, setError] = createSignal("");
  const [drafts, setDrafts] = createSignal<Record<RuleKind, string>>({ ask: "", allow: "" });
  onSettled(() => {
    api.getPermissions().then(setPermissions, (e) => setError(errorText(e)));
  });

  const save = async (next: Permissions) => {
    const previous = permissions();
    setPermissions(next);
    setError("");
    try {
      setPermissions(await api.savePermissions(next));
    } catch (e) {
      setPermissions(previous);
      setError(errorText(e));
    }
  };

  const addRule = (kind: RuleKind, e: Event) => {
    e.preventDefault();
    const pattern = drafts()[kind].trim();
    const current = permissions();
    if (!pattern || !current) return;
    setDrafts((d) => ({ ...d, [kind]: "" }));
    void save({ ...current, rules: [...current.rules, { kind, pattern }] });
  };

  const removeRule = (index: number) => {
    const current = permissions()!;
    void save({ ...current, rules: current.rules.filter((_, i) => i !== index) });
  };

  return (
    <Page title="Permissions">
      <div class="settings">
        <Show when={permissions()}>
          {(p) => (
            <>
              <div class="list">
                <div class="list-row settings-row">
                  <div class="list-main">
                    <div class="list-title">Approvals</div>
                    <p class="settings-detail">{MODES.find((m) => m.value === p().mode)?.about}</p>
                  </div>
                  <select
                    class="permissions-mode"
                    aria-label="Approvals"
                    value={p().mode}
                    onChange={(e) => void save({ ...p(), mode: e.currentTarget.value as PermissionMode })}
                  >
                    <For each={MODES}>{(m) => <option value={m.value}>{m.label}</option>}</For>
                  </select>
                </div>
              </div>
              <p class="hint settings-footnote">
                Codex runs commands it knows only read, like <code>ls</code> or <code>git status</code>, without asking. Claude checks with Grimoire before every shell
                command.
              </p>

              <For each={SECTIONS}>
                {(section) => {
                  const rules = () => p().rules.map((rule, index) => ({ rule, index })).filter(({ rule }) => rule.kind === section.kind);
                  const unused = () => (section.kind === "ask" ? p().mode === "off" : p().mode !== "always");
                  return (
                    <>
                      <h2 class="section-title">{section.title}</h2>
                      <div class={["list", "permissions-rules", { unused: unused() }]}>
                        <For each={rules()}>
                          {({ rule, index }) => (
                            <div class="list-row permissions-rule">
                              <code>{rule.pattern}</code>
                              <button type="button" class="icon-button" aria-label={`Remove ${rule.pattern}`} title="Remove" onClick={() => removeRule(index)}>
                                <CloseIcon />
                              </button>
                            </div>
                          )}
                        </For>
                        <form class="list-row permissions-add" onSubmit={(e) => addRule(section.kind, e)}>
                          <input
                            aria-label={`New ${section.title.toLowerCase()} rule`}
                            placeholder={section.example}
                            spellcheck={false}
                            value={drafts()[section.kind]}
                            onInput={(e) => setDrafts((d) => ({ ...d, [section.kind]: e.currentTarget.value }))}
                          />
                          <button type="submit" class="ghost small" disabled={!drafts()[section.kind].trim()}>
                            Add rule
                          </button>
                        </form>
                      </div>
                      <p class="hint settings-footnote">
                        {section.about} <code>*</code> matches anything.
                        <Show when={unused()}> These rules don't apply with "{MODES.find((m) => m.value === p().mode)?.label}".</Show>
                      </p>
                    </>
                  );
                }}
              </For>

              <DelegationSettings />

              <h2 class="section-title">Vault</h2>
              <div class="list">
                <div class="list-row">
                  <div class="list-main">
                    <div class="list-title">Secrets</div>
                    <p class="settings-detail">
                      Familiars always ask before using a vault item, unless you chose Always allow for it. Each item's page shows who can.
                    </p>
                  </div>
                  <button type="button" class="ghost small" onClick={props.onOpenVault}>
                    Open vault
                  </button>
                </div>
              </div>
            </>
          )}
        </Show>
        <Show when={error()}>
          <p class="error-box settings-error" role="alert">
            {error()}
          </p>
        </Show>
      </div>
    </Page>
  );
}
