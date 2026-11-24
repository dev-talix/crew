import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type Agent } from "./api";
import { addedFor, connectorStatus, type CatalogEntry, type Connector } from "./connectors";
import { Page, Sheet } from "./ui";
import "./connectors.css";

function Tile(props: { name: string; on?: boolean }) {
  return (
    <span class={["connector-tile", { on: props.on }]} aria-hidden="true">
      {props.name.charAt(0).toUpperCase()}
    </span>
  );
}

export default function ConnectorsPage(props: { agents: Agent[] }) {
  const [connectors, setConnectors] = createSignal<Connector[]>([]);
  const [catalog, setCatalog] = createSignal<CatalogEntry[]>([]);
  const [error, setError] = createSignal("");
  // The connector whose browser sign-in is under way.
  const [signingIn, setSigningIn] = createSignal<string | null>(null);
  const [tokenFor, setTokenFor] = createSignal<Connector | null>(null);
  const [confirmRemove, setConfirmRemove] = createSignal<string | null>(null);
  const [url, setUrl] = createSignal("");
  const [name, setName] = createSignal("");
  const [adding, setAdding] = createSignal(false);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));

  const load = () => api.listConnectors().then(setConnectors, (e) => setError(errorText(e)));
  onMountAsync(async (keep) => {
    keep(await onEvent("connectors_changed", () => void load()));
    keep(await onEvent("agents_changed", () => void load()));
    setCatalog(await api.connectorCatalog());
    await load();
  });

  const names = createMemo(() => Object.fromEntries(props.agents.map((a) => [a.id, a.name])));
  const available = createMemo(() => catalog().filter((e) => !addedFor(e, connectors())));
  const entryFor = (c: Connector) => catalog().find((e) => addedFor(e, [c]));

  const run = async (action: () => Promise<unknown>) => {
    setError("");
    try {
      await action();
      await load();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const signIn = (c: Connector) =>
    run(async () => {
      setSigningIn(c.id);
      try {
        await api.connectorSignIn(c.id);
      } finally {
        setSigningIn(null);
      }
    });

  const remove = (c: Connector) => {
    if (confirmRemove() !== c.id) {
      setConfirmRemove(c.id);
      clearTimeout(confirmTimer);
      confirmTimer = window.setTimeout(() => setConfirmRemove(null), 4000);
      return;
    }
    setConfirmRemove(null);
    void run(() => api.removeConnector(c.id));
  };

  const addByUrl = (e: SubmitEvent) => {
    e.preventDefault();
    setAdding(true);
    void run(async () => {
      await api.addConnector({ url: url(), name: name().trim() || undefined });
      setUrl("");
      setName("");
    }).finally(() => setAdding(false));
  };

  return (
    <Page title="Connectors">
      <div class="settings connectors">
        <p class="settings-detail">
          Hosted MCP servers, like Linear or Notion, that familiars can use once you sign in here. You sign in with your own account in the
          browser, so you don't need a developer account. Turn a connector on for each familiar in that familiar's settings. Sign-ins stay in
          your Keychain, and familiars never see them.
        </p>
        <Show when={error()}>
          <p class="error-box settings-error" role="alert">
            {error()}
          </p>
        </Show>

        <Show when={connectors().length > 0}>
          <h2 class="section-title">Added</h2>
          <div class="list">
            <For each={connectors()}>
              {(c) => (
                <div class="list-row connector-row">
                  <Tile name={c.name} on={!!c.auth} />
                  <div class="list-main">
                    <span class="list-title">{c.name}</span>
                    <span class="list-sub" title={c.url}>
                      {signingIn() === c.id ? "Finish signing in in your browser…" : connectorStatus(c, names())}
                    </span>
                  </div>
                  <div class="list-actions">
                    <Show
                      when={c.auth}
                      fallback={
                        <>
                          <button type="button" class="ghost small" onClick={() => setTokenFor(c)}>
                            Use a token
                          </button>
                          <Show when={!entryFor(c)?.tokenHelp}>
                            <button type="button" class="primary small" disabled={signingIn() !== null} onClick={() => void signIn(c)}>
                              Sign in
                            </button>
                          </Show>
                        </>
                      }
                    >
                      <button type="button" class="ghost small" onClick={() => void run(() => api.connectorSignOut(c.id))}>
                        Sign out
                      </button>
                    </Show>
                    <button type="button" class="ghost small danger" onClick={() => remove(c)}>
                      {confirmRemove() === c.id ? "Click again" : "Remove"}
                    </button>
                  </div>
                </div>
              )}
            </For>
          </div>
        </Show>

        <Show when={available().length > 0}>
          <h2 class="section-title">Catalog</h2>
          <div class="list">
            <For each={available()}>
              {(entry) => (
                <div class="list-row connector-row">
                  <Tile name={entry.name} />
                  <div class="list-main">
                    <span class="list-title">{entry.name}</span>
                    <span class="list-sub">{entry.tokenHelp ? `${entry.description}. Uses a personal access token.` : entry.description}</span>
                  </div>
                  <button type="button" class="ghost small" onClick={() => void run(() => api.addConnector({ catalogId: entry.id }))}>
                    Add
                  </button>
                </div>
              )}
            </For>
          </div>
        </Show>

        <h2 class="section-title">Add by URL</h2>
        <form class="list connector-url" onSubmit={addByUrl}>
          <div class="list-row">
            <label class="field">
              <span>Server URL</span>
              <input
                value={url()}
                onInput={(e) => setUrl(e.currentTarget.value)}
                placeholder="https://mcp.example.com/mcp"
                autocapitalize="off"
                autocorrect="off"
                spellcheck={false}
                required
              />
            </label>
            <label class="field connector-name">
              <span>Name</span>
              <input value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="Optional" maxlength={40} />
            </label>
            <button type="submit" class="ghost" disabled={adding() || !url().trim()}>
              Add
            </button>
          </div>
        </form>
        <p class="hint settings-footnote">
          Grimoire signs in to servers that let apps register themselves. Services that only work with your own OAuth app, like Gmail or Slack,
          aren't supported. For a server that takes a personal access token instead, use "Use a token".
        </p>
      </div>

      <Show when={tokenFor()}>
        {(c) => (
          <Sheet label={`Token for ${c().name}`} onClose={() => setTokenFor(null)}>
            <TokenForm connector={c()} help={entryFor(c())?.tokenHelp ?? null} onDone={() => (setTokenFor(null), void load())} />
          </Sheet>
        )}
      </Show>
    </Page>
  );
}

function TokenForm(props: { connector: Connector; help: string | null; onDone: () => void }) {
  const [token, setToken] = createSignal("");
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const save = async (e: SubmitEvent) => {
    e.preventDefault();
    setSaving(true);
    setError("");
    try {
      await api.connectorSetToken(props.connector.id, token());
      setToken("");
      props.onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };
  return (
    <form class="connector-token" onSubmit={save}>
      <h1>Token for {props.connector.name}</h1>
      <p class="hint">
        {props.help ?? "Paste an access token for this server. Grimoire sends it as a bearer token."} It's saved in your Keychain, and familiars
        never see it.
      </p>
      <label class="field">
        <span>Access token</span>
        <input type="password" value={token()} onInput={(e) => setToken(e.currentTarget.value)} autocomplete="off" required />
      </label>
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
        <button type="submit" class="primary" disabled={saving() || !token().trim()}>
          Save token
        </button>
      </div>
    </form>
  );
}
