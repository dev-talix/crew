import { createSignal, For, Show } from "solid-js";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, createFetch, describeSchedule, errorText, type Access, type Provider } from "./api";
import { addedFor, suggestConnectors, type CatalogEntry } from "./connectors";
import { randomAvatar } from "./avatar";
import { MEMORY_LIMIT, memoryUsage } from "./memory";
import { Segmented, Sheet, Toggle } from "./ui";
import { draftFromTemplate, matchesSearch, memoryFit, permissionLines, type MarketTemplate, type MemoryRow, type RitualRow } from "./templates";
import "./templates.css";

const ORIGIN: Record<MemoryRow["origin"], string> = {
  description: "Description",
  instructions: "Instructions",
  memory: "Memory",
  skill: "Skill",
  integrations: "Apps",
};

/** Paste a Grok Bot marketplace link, or browse the marketplace, then review and edit before anything is created. */
export default function TemplateImport(props: { onClose: () => void; onCreated: (agentId: string) => void }) {
  const [mode, setMode] = createSignal<"link" | "browse">("link");
  const [link, setLink] = createSignal("");
  const [catalog, setCatalog] = createSignal<MarketTemplate[] | null>(null);
  const [query, setQuery] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [template, setTemplate] = createSignal<MarketTemplate | null>(null);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const load = (url: string) => run(async () => {
      setTemplate(await api.marketplaceTemplate(url));
    });
  const browse = () => {
    setMode("browse");
    if (!catalog()) void run(async () => {
        setCatalog(await api.marketplaceCatalog());
      });
  };
  const shown = () => (catalog() ?? []).filter((t) => matchesSearch(t, query()));

  return (
    <Sheet label="Import a template" wide onClose={props.onClose}>
      <Show
        when={template()}
        fallback={
          <div class="wf-form template-pick">
            <h1>Import a Grok Bot template</h1>
            <Segmented
              label="How to find it"
              value={mode()}
              options={[
                { value: "link", label: "Paste a link" },
                { value: "browse", label: "Browse" },
              ]}
              onChange={(next) => (next === "browse" ? browse() : setMode("link"))}
            />
            <Show
              when={mode() === "browse"}
              fallback={
                <form
                  class="template-link"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void load(link());
                  }}
                >
                  <label class="field">
                    <span>Marketplace or share link</span>
                    <input value={link()} onInput={(e) => setLink(e.currentTarget.value)} placeholder="https://x.ai/bot/marketplace/bots/haggle-bot" autofocus />
                  </label>
                  <button class="primary" type="submit" disabled={busy() || !link().trim()}>
                    {busy() ? "Reading..." : "Read template"}
                  </button>
                </form>
              }
            >
              <input class="template-search" type="search" placeholder="Search templates" aria-label="Search templates" value={query()} onInput={(e) => setQuery(e.currentTarget.value)} />
              <Show when={catalog()} fallback={<p class="hint">{busy() ? "Reading the marketplace..." : ""}</p>}>
                <ul class="list template-list">
                  <For each={shown()} fallback={<li class="list-row"><p class="hint">No templates match.</p></li>}>
                    {(t) => (
                      <li>
                        <button type="button" class="list-row template-card" disabled={busy()} onClick={() => void load(t.sourceUrl)}>
                          <div class="list-main">
                            <div class="list-title">
                              {t.name} <span class="template-by">by {t.creator}</span>
                            </div>
                            <p class="list-sub">{t.description}</p>
                          </div>
                          <Show when={t.categories[0]}>
                            <span class="import-tag">{t.categories[0]}</span>
                          </Show>
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </Show>
            <p class="hint">
              Grimoire reads the public marketplace page at x.ai/bot/marketplace. The marketplace has no public API, so this can stop working when x.ai changes its pages. Private bots
              and share links that aren't in the marketplace can't be imported.
            </p>
            <Show when={error()}>
              <p class="error-box" role="alert">
                {error()}
              </p>
            </Show>
          </div>
        }
      >
        {(t) => <TemplateReview template={t()} onBack={() => setTemplate(null)} onClose={props.onClose} onCreated={props.onCreated} />}
      </Show>
    </Sheet>
  );
}

function TemplateReview(props: { template: MarketTemplate; onBack: () => void; onClose: () => void; onCreated: (agentId: string) => void }) {
  const start = draftFromTemplate(props.template);
  const [name, setName] = createSignal(start.name);
  // Codex by default: its workspace access is an OS sandbox, the tightest fit for text someone else wrote.
  const [provider, setProvider] = createSignal<Provider>("codex");
  const [access, setAccess] = createSignal<Access>("workspace");
  const [memory, setMemory] = createSignal<MemoryRow[]>(start.memory);
  const [rituals, setRituals] = createSignal<RitualRow[]>(start.rituals);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const avatar = randomAvatar();
  const catalog = createFetch(() => null, api.connectorCatalog);
  // Catalog connectors for the apps the template names; the person can untick any.
  const suggested = () => suggestConnectors(props.template.integrations.map((i) => i.name), catalog() ?? []);
  const [skipped, setSkipped] = createSignal<string[]>([]);
  // The template's app names that a checked connector covers.
  const connectable = () =>
    props.template.integrations
      .map((i) => i.name)
      .filter((app) => suggestConnectors([app], suggested()).some((e) => !skipped().includes(e.id)));

  const editMemory = (id: string, change: Partial<MemoryRow>) => setMemory((rows) => rows.map((r) => (r.id === id ? { ...r, ...change } : r)));
  const editRitual = (id: string, change: Partial<RitualRow>) => setRituals((rows) => rows.map((r) => (r.id === id ? { ...r, ...change } : r)));
  const fit = () => memoryFit(memory());
  const usage = () => memoryUsage(fit().bytes, MEMORY_LIMIT);

  const create = async (e: SubmitEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    try {
      const id = await api.importTemplate({
        name: name(),
        provider: provider(),
        model: null,
        effort: null,
        access: access(),
        avatar,
        memory: memory()
          .filter((r) => r.include)
          .map((r) => ({ kind: r.kind, text: r.text })),
        rituals: rituals()
          .filter((r) => r.include)
          .map((r) => ({ name: r.name, prompt: r.prompt, schedule: r.schedule })),
      });
      // The familiar exists now, so a connector that fails to add can't block opening it; it can be
      // turned on later in its settings. Retrying the import would make a second familiar.
      await connect(id, suggested().filter((e) => !skipped().includes(e.id))).catch(() => undefined);
      props.onCreated(id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  /** Adds each chosen connector that isn't added yet and turns them on for the new familiar. */
  const connect = async (agentId: string, chosen: CatalogEntry[]) => {
    if (!chosen.length) return;
    const existing = await api.listConnectors();
    const ids: string[] = [];
    for (const entry of chosen) ids.push((addedFor(entry, existing) ?? (await api.addConnector({ catalogId: entry.id }))).id);
    await api.setAgentConnectors(agentId, ids);
  };

  return (
    <form class="wf-form template-review" onSubmit={create}>
      <h1>Review {props.template.name}</h1>
      <p class="hint">
        By {props.template.creator || "someone"} on the{" "}
        <a href={props.template.sourceUrl} onClick={(e) => (e.preventDefault(), void openUrl(props.template.sourceUrl))}>
          Grok Bot marketplace
        </a>
        . Someone else wrote this text. Read it and change anything before you create the familiar.
      </p>

      <div class="form-grid">
        <label class="field">
          <span>Name</span>
          <input value={name()} onInput={(e) => setName(e.currentTarget.value)} required maxlength={60} />
        </label>
        <div class="field">
          <span>Provider</span>
          <Segmented
            label="Provider"
            value={provider()}
            options={[
              { value: "codex", label: "Codex" },
              { value: "claude", label: "Claude" },
            ]}
            onChange={setProvider}
          />
        </div>
      </div>

      <section class="template-asks">
        <h2 class="section-title">What it asks for</h2>
        <ul>
          <For each={permissionLines(props.template, access(), provider(), connectable())}>{(line) => <li>{line}</li>}</For>
        </ul>
        <div class="list">
          <div class="list-row">
            <div class="list-main">
              <div class="list-title">Full access</div>
              <p class="list-sub">
                {provider() === "codex" ? "Off keeps it in Codex's workspace sandbox." : "Off limits Claude's file edits to its workspace."} Turn it on only if you trust every rule below.
              </p>
            </div>
            <Toggle label="Full access" checked={access() === "full"} onChange={(on) => setAccess(on ? "full" : "workspace")} />
          </div>
        </div>
      </section>

      <Show when={suggested().length}>
        <section>
          <h2 class="section-title">Connectors</h2>
          <p class="hint">Grimoire can connect some of the apps this template names. Checked ones are added and turned on for this familiar. Sign in to each in Settings, under Connectors.</p>
          <ul class="list template-rows">
            <For each={suggested()}>
              {(entry) => (
                <li class={["list-row template-row", { off: skipped().includes(entry.id) }]}>
                  <input
                    type="checkbox"
                    checked={!skipped().includes(entry.id)}
                    aria-label={`Connect ${entry.name}`}
                    onChange={(e) => setSkipped((ids) => (e.currentTarget.checked ? ids.filter((x) => x !== entry.id) : [...ids, entry.id]))}
                  />
                  <div class="list-main">
                    <span class="list-title">{entry.name}</span>
                    <span class="list-sub">{entry.description}</span>
                  </div>
                </li>
              )}
            </For>
          </ul>
        </section>
      </Show>

      <section>
        <h2 class="section-title">
          Memory <span class={["template-usage", { over: !fit().fits }]}>{usage().label}</span>
        </h2>
        <p class="hint">Grimoire tells a familiar its rules and facts at the start of every chat. Skills come in as rules, since Grimoire can't run Grok skills.</p>
        <ul class="list template-rows">
          <For each={memory().map((r) => r.id)}>
            {(id) => {
              const row = () => memory().find((r) => r.id === id)!;
              return (
                <li class={["list-row template-row", { off: !row().include }]}>
                  <input type="checkbox" checked={row().include} aria-label="Include" onChange={(e) => editMemory(row().id, { include: e.currentTarget.checked })} />
                  <div class="list-main">
                    <div class="template-row-head">
                      <span class="import-tag">{ORIGIN[row().origin]}</span>
                      <select value={row().kind} aria-label="Kind" onChange={(e) => editMemory(row().id, { kind: e.currentTarget.value as MemoryRow["kind"] })}>
                        <option value="rule">Rule</option>
                        <option value="fact">Fact</option>
                      </select>
                      <Show when={row().text.trim().length > 1000}>
                        <span class="template-warn">{row().text.trim().length} of 1000 characters</span>
                      </Show>
                    </div>
                    <textarea rows={3} value={row().text} aria-label="Memory text" onInput={(e) => editMemory(row().id, { text: e.currentTarget.value })} />
                  </div>
                </li>
              );
            }}
          </For>
        </ul>
      </section>

      <Show when={rituals().length}>
        <section>
          <h2 class="section-title">Rituals</h2>
          <p class="hint">They come in turned off. Turn each on in Rituals once its schedule looks right.</p>
          <ul class="list template-rows">
              <For each={rituals().map((r) => r.id)}>
              {(id) => {
                const row = () => rituals().find((r) => r.id === id)!;
                return (
                  <li class={["list-row template-row", { off: !row().include }]}>
                    <input type="checkbox" checked={row().include} aria-label="Include" onChange={(e) => editRitual(row().id, { include: e.currentTarget.checked })} />
                    <div class="list-main">
                      <input class="template-ritual-name" value={row().name} maxlength={60} aria-label="Ritual name" onInput={(e) => editRitual(row().id, { name: e.currentTarget.value })} />
                      <textarea rows={2} value={row().prompt} aria-label="Ritual prompt" onInput={(e) => editRitual(row().id, { prompt: e.currentTarget.value })} />
                      <p class="list-sub">
                        {describeSchedule(row().schedule)}. {row().scheduleNote}
                      </p>
                    </div>
                  </li>
                );
              }}
            </For>
          </ul>
        </section>
      </Show>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>
      <div class="form-actions">
        <button type="button" class="ghost" onClick={props.onBack}>
          Back
        </button>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={props.onClose}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={busy() || !name().trim() || !fit().fits}>
          {busy() ? "Creating..." : "Create familiar"}
        </button>
      </div>
    </form>
  );
}
