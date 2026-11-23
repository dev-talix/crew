import { createMemo, createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, relativeTime, type Agent, type VaultActivity, type VaultItem, type VaultKind } from "./api";
import { CardIcon, CopyIcon, KeyIcon, PlusIcon, SearchIcon } from "./icons";
import { EmptyState, Page, Sheet } from "./ui";
import "./vault.css";

const KINDS: { kind: VaultKind; label: string; plural: string }[] = [
  { kind: "login", label: "Login", plural: "Logins" },
  { kind: "api_key", label: "API key", plural: "API keys" },
  { kind: "card", label: "Card", plural: "Cards" },
];

const FIELD_LABELS: Record<string, string> = { password: "Password", key: "Key", number: "Card number", expiry: "Expiry", cvc: "Security code" };
const SECRET_FIELDS: Record<VaultKind, string[]> = { login: ["password"], api_key: ["key"], card: ["number", "expiry", "cvc"] };
const kindLabel = (kind: VaultKind) => KINDS.find((k) => k.kind === kind)!.label;

/** What the list shows under an item's name. */
const subtitle = (item: VaultItem) => (item.kind === "card" ? [item.username, item.hint].filter(Boolean).join("  ") : item.username ?? item.site ?? kindLabel(item.kind));

function ItemTile(props: { item: VaultItem }) {
  const letter = () => (props.item.site ?? props.item.name).replace(/^https?:\/\/(www\.)?/, "").charAt(0).toUpperCase();
  return (
    <span class={["vault-tile", `vault-tile-${props.item.kind}`]} aria-hidden="true">
      <Show when={props.item.kind === "login"} fallback={props.item.kind === "card" ? <CardIcon /> : <KeyIcon />}>
        {letter()}
      </Show>
    </span>
  );
}

async function copy(text: string) {
  await navigator.clipboard.writeText(text);
}

export default function VaultPage(props: { agents: Agent[] }) {
  const [items, setItems] = createSignal<VaultItem[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [filter, setFilter] = createSignal<VaultKind | "all">("all");
  const [query, setQuery] = createSignal("");
  const [open, setOpen] = createSignal<VaultItem | null>(null);
  const [editing, setEditing] = createSignal<{ kind: VaultKind; item: VaultItem | null } | null>(null);
  const [menu, setMenu] = createSignal(false);

  const load = () =>
    api.listVault().then(
      (list) => {
        setItems(list);
        setLoaded(true);
        // Keep an open item in step with edits, or close it once it's gone.
        const current = open();
        if (current) setOpen(list.find((i) => i.id === current.id) ?? null);
      },
      (e) => setError(errorText(e)),
    );
  onMountAsync(async (keep) => {
    keep(await onEvent("vault_changed", () => void load()));
    await load();
  });

  const count = (kind: VaultKind | "all") => (kind === "all" ? items().length : items().filter((i) => i.kind === kind).length);
  const shown = createMemo(() => {
    const q = query().trim().toLowerCase();
    return items().filter(
      (i) =>
        (filter() === "all" || i.kind === filter()) &&
        (!q || [i.name, i.site, i.username].some((text) => text?.toLowerCase().includes(q))),
    );
  });

  const start = (kind: VaultKind) => {
    setMenu(false);
    setEditing({ kind, item: null });
  };

  const newButton = (
    <div class="vault-new">
      <button class="primary" aria-haspopup="menu" aria-expanded={menu() ? "true" : "false"} onClick={() => setMenu(!menu())}>
        <PlusIcon /> New item
      </button>
      <Show when={menu()}>
        <div class="vault-menu" role="menu">
          <For each={KINDS}>
            {(k) => (
              <button role="menuitem" onClick={() => start(k.kind)}>
                {k.label}
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );

  return (
    <Page title="Vault" actions={newButton}>
      <div class="vault">
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>
        <Show
          when={!loaded() || items().length > 0}
          fallback={
            <EmptyState
              title="Your vault is empty"
              body="Save logins, API keys, and cards. Familiars can use them in commands without ever seeing them, and Grimoire asks you first."
            />
          }
        >
          <div class="vault-toolbar">
            <div class="vault-filters" role="tablist" aria-label="Item kinds">
              <For each={[{ kind: "all" as const, plural: "All" }, ...KINDS]}>
                {(f) => (
                  <Show when={f.kind === "all" || count(f.kind) > 0}>
                    <button role="tab" aria-selected={filter() === f.kind ? "true" : "false"} class={{ on: filter() === f.kind }} onClick={() => setFilter(f.kind)}>
                      {f.plural} <span class="vault-count">{count(f.kind)}</span>
                    </button>
                  </Show>
                )}
              </For>
            </div>
            <label class="vault-search">
              <SearchIcon />
              <input placeholder="Search the vault" aria-label="Search the vault" value={query()} onInput={(e) => setQuery(e.currentTarget.value)} />
            </label>
          </div>
          <div class="list vault-list">
            <For each={shown()} fallback={<p class="vault-none">Nothing matches "{query()}".</p>}>
              {(item) => (
                <button class="list-row vault-row" onClick={() => setOpen(item)}>
                  <ItemTile item={item} />
                  <span class="list-main">
                    <span class="list-title">{item.name}</span>
                    <span class="list-sub">{subtitle(item)}</span>
                  </span>
                  <span class="vault-chevron" aria-hidden="true">
                    ›
                  </span>
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>

      <Show when={open()}>
        {(item) => (
          <Sheet label={item().name} onClose={() => setOpen(null)}>
            <ItemDetails
              item={item()}
              agents={props.agents}
              onEdit={() => setEditing({ kind: item().kind, item: item() })}
              onClose={() => setOpen(null)}
            />
          </Sheet>
        )}
      </Show>
      <Show when={editing()}>
        {(target) => (
          <Sheet label={target().item ? "Edit item" : `New ${kindLabel(target().kind).toLowerCase()}`} onClose={() => setEditing(null)}>
            <ItemEditor kind={target().kind} item={target().item} onDone={() => setEditing(null)} />
          </Sheet>
        )}
      </Show>
    </Page>
  );
}

function ItemDetails(props: { item: VaultItem; agents: Agent[]; onEdit: () => void; onClose: () => void }) {
  // Secrets load only when asked for, and leave with the sheet.
  const [secret, setSecret] = createSignal<Record<string, string> | null>(null);
  const [revealed, setRevealed] = createSignal<string[]>([]);
  const [copied, setCopied] = createSignal("");
  const [activity, setActivity] = createSignal<VaultActivity | null>(null);
  const [error, setError] = createSignal("");
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  let timer: number | undefined;
  onCleanup(() => clearTimeout(timer));

  const loadActivity = () => api.vaultActivity(props.item.id).then(setActivity, (e) => setError(errorText(e)));
  onMountAsync(async (keep) => {
    keep(await onEvent("vault_changed", () => void loadActivity()));
    await loadActivity();
  });

  const value = async (field: string) => {
    const known = secret() ?? (await api.vaultSecret(props.item.id));
    setSecret(known);
    return known[field] ?? "";
  };
  const reveal = (field: string) =>
    value(field).then(
      () => setRevealed((list) => (list.includes(field) ? list.filter((f) => f !== field) : [...list, field])),
      (e) => setError(errorText(e)),
    );
  const copyField = (field: string, text?: string) =>
    (text !== undefined ? Promise.resolve(text) : value(field))
      .then(copy)
      .then(() => {
        setCopied(field);
        clearTimeout(timer);
        timer = window.setTimeout(() => setCopied(""), 1500);
      })
      .catch((e) => setError(errorText(e)));

  const nameOf = (id: string | null) => props.agents.find((a) => a.id === id)?.name ?? "A deleted familiar";
  const remove = async () => {
    if (!confirmDelete()) return setConfirmDelete(true);
    try {
      await api.deleteVaultItem(props.item.id);
      props.onClose();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const CopyButton = (p: { field: string; text?: string; label: string }) => (
    <button type="button" class="icon-button" aria-label={`Copy ${p.label.toLowerCase()}`} title="Copy" onClick={() => void copyField(p.field, p.text)}>
      <Show when={copied() === p.field} fallback={<CopyIcon />}>
        <span class="vault-copied">Copied</span>
      </Show>
    </button>
  );

  return (
    <div class="vault-details">
      <header class="vault-details-head">
        <ItemTile item={props.item} />
        <div>
          <h1>{props.item.name}</h1>
          <p class="list-sub">{kindLabel(props.item.kind)}</p>
        </div>
      </header>

      <dl class="vault-fields">
        <Show when={props.item.site}>
          <dt>{props.item.kind === "api_key" ? "Service" : "Site"}</dt>
          <dd>{props.item.site}</dd>
        </Show>
        <Show when={props.item.username}>
          {(username) => (
            <>
              <dt>{props.item.kind === "card" ? "Name on card" : "Username"}</dt>
              <dd>
                <span class="vault-value">{username()}</span>
                <CopyButton field="username" text={username()} label="Username" />
              </dd>
            </>
          )}
        </Show>
        <For each={SECRET_FIELDS[props.item.kind]}>
          {(field) => {
            const shown = () => revealed().includes(field);
            const hidden = () => (field === "number" && props.item.hint ? props.item.hint : "••••••••••");
            return (
              <Show when={field === SECRET_FIELDS[props.item.kind][0] || !secret() || secret()![field]}>
                <dt>{FIELD_LABELS[field]}</dt>
                <dd>
                  <span class={["vault-value", { secret: !shown() }]}>{shown() ? secret()?.[field] ?? "" : hidden()}</span>
                  <button type="button" class="ghost small" onClick={() => void reveal(field)}>
                    {shown() ? "Hide" : "Show"}
                  </button>
                  <CopyButton field={field} label={FIELD_LABELS[field]} />
                </dd>
              </Show>
            );
          }}
        </For>
      </dl>

      <h2 class="section-title">Familiars</h2>
      <Show
        when={activity()?.alwaysAllowed.length}
        fallback={<p class="hint">Familiars ask you before using this item. Nobody is allowed without asking.</p>}
      >
        <div class="list">
          <For each={activity()?.alwaysAllowed ?? []}>
            {(agentId) => (
              <div class="list-row vault-grant">
                <span class="list-main">
                  <span class="list-title">{nameOf(agentId)}</span>
                  <span class="list-sub">Uses this item without asking</span>
                </span>
                <button class="ghost small" onClick={() => api.revokeVaultGrant(props.item.id, agentId).catch((e) => setError(errorText(e)))}>
                  Ask again
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={activity()?.uses.length}>
        <h2 class="section-title">Recent uses</h2>
        <ul class="vault-uses">
          <For each={activity()?.uses ?? []}>
            {(use) => (
              <li>
                <span>
                  {nameOf(use.agentId)} {use.outcome === "allowed" ? "used" : "was denied"} the {FIELD_LABELS[use.field]?.toLowerCase() ?? use.field}
                </span>
                <time>{relativeTime(use.at)}</time>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>
      <div class="form-actions">
        <button type="button" class="ghost danger" onClick={() => void remove()}>
          {confirmDelete() ? "Click again to delete" : "Delete"}
        </button>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={props.onClose}>
          Close
        </button>
        <button type="button" class="primary" onClick={props.onEdit}>
          Edit
        </button>
      </div>
    </div>
  );
}

/** A random password from letters, digits, and a few symbols that survive shells and forms. */
function generatePassword(length = 20): string {
  const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789-_.!@#%";
  const bytes = crypto.getRandomValues(new Uint32Array(length));
  return Array.from(bytes, (b) => alphabet[b % alphabet.length]).join("");
}

function ItemEditor(props: { kind: VaultKind; item: VaultItem | null; onDone: () => void }) {
  const editing = !!props.item;
  const [name, setName] = createSignal(props.item?.name ?? "");
  const [site, setSite] = createSignal(props.item?.site ?? "");
  const [username, setUsername] = createSignal(props.item?.username ?? "");
  const [secret, setSecret] = createSignal<Record<string, string>>({});
  const [showSecret, setShowSecret] = createSignal(false);
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  let nameField!: HTMLInputElement;
  onSettled(() => nameField.focus());

  const setField = (field: string, value: string) => setSecret((s) => ({ ...s, [field]: value }));

  const save = async (e: Event) => {
    e.preventDefault();
    setSaving(true);
    setError("");
    // On an edit, an empty secret field means "keep it", so only typed fields are sent.
    const typed = Object.fromEntries(Object.entries(secret()).filter(([, v]) => v.trim()));
    try {
      await api.saveVaultItem({
        id: props.item?.id ?? null,
        kind: props.kind,
        name: name(),
        site: site() || null,
        username: username() || null,
        secret: typed,
      });
      props.onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const secretInput = (field: string, placeholder: string, extra?: { inputmode?: "numeric"; maxlength?: number }) => (
    <label class="field">
      <span>{FIELD_LABELS[field]}</span>
      <input
        type={showSecret() || field === "expiry" ? "text" : "password"}
        autocomplete="off"
        spellcheck={false}
        value={secret()[field] ?? ""}
        placeholder={editing ? "Unchanged" : placeholder}
        inputmode={extra?.inputmode}
        maxlength={extra?.maxlength}
        onInput={(e) => setField(field, e.currentTarget.value)}
      />
    </label>
  );

  return (
    <form class="vault-editor" onSubmit={save}>
      <h1>{editing ? `Edit ${props.item!.name}` : `New ${kindLabel(props.kind).toLowerCase()}`}</h1>
      <label class="field">
        <span>Name</span>
        <input
          ref={nameField}
          required
          maxlength={80}
          value={name()}
          placeholder={props.kind === "card" ? "Amex Gold" : props.kind === "api_key" ? "OpenAI" : "GitHub"}
          onInput={(e) => setName(e.currentTarget.value)}
        />
      </label>
      <Show when={props.kind !== "card"}>
        <label class="field">
          <span>{props.kind === "api_key" ? "Service" : "Site"}</span>
          <input value={site()} placeholder={props.kind === "api_key" ? "api.openai.com" : "github.com"} onInput={(e) => setSite(e.currentTarget.value)} />
        </label>
      </Show>
      <Show when={props.kind !== "api_key"}>
        <label class="field">
          <span>{props.kind === "card" ? "Name on card" : "Username or email"}</span>
          <input value={username()} autocomplete="off" onInput={(e) => setUsername(e.currentTarget.value)} />
        </label>
      </Show>

      <Show when={props.kind === "login"}>
        <div class="vault-secret-row">
          {secretInput("password", "Required")}
          <button type="button" class="ghost small" onClick={() => (setField("password", generatePassword()), setShowSecret(true))}>
            Generate
          </button>
        </div>
      </Show>
      <Show when={props.kind === "api_key"}>{secretInput("key", "Required")}</Show>
      <Show when={props.kind === "card"}>
        {secretInput("number", "1234 5678 9012 3456", { inputmode: "numeric" })}
        <div class="vault-pair">
          {secretInput("expiry", "MM/YY", { maxlength: 7 })}
          {secretInput("cvc", "123", { inputmode: "numeric", maxlength: 4 })}
        </div>
      </Show>
      <label class="vault-show">
        <input type="checkbox" checked={showSecret()} onChange={(e) => setShowSecret(e.currentTarget.checked)} /> Show what I type
      </label>
      <p class="hint">Secrets are kept in your macOS Keychain. Grimoire's own database only stores the name, site, and username.</p>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>
      <div class="form-actions">
        <span class="spacer" />
        <button type="button" class="ghost" onClick={props.onDone}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={saving() || !name().trim()}>
          {editing ? "Save changes" : "Save to vault"}
        </button>
      </div>
    </form>
  );
}
