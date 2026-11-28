/** A hosted MCP server Grimoire knows, as `connector_catalog` lists it. */
export interface CatalogEntry {
  id: string;
  name: string;
  url: string;
  description: string;
  /** Lowercase app names a template might use for it. */
  apps: string[];
  /** Set when an app name is also a common word, so a template's app name must equal one of `apps`. */
  exactApps: boolean;
  /** Set when the server can't sign in through the browser, so the person pastes a token. */
  tokenHelp: string | null;
}

/** A connector the person added. Tokens stay in the Keychain and never reach the webview. */
export interface Connector {
  id: string;
  name: string;
  url: string;
  catalogId: string | null;
  /** The MCP server name familiars see. */
  key: string;
  auth: "oauth" | "token" | null;
  /** Familiars that have it turned on. */
  agents: string[];
}

export interface NewConnector {
  catalogId?: string;
  url?: string;
  name?: string;
}

const normalize = (name: string) => name.toLowerCase().replace(/[^a-z0-9.]+/g, " ").trim();

/**
 * Catalog entries for the apps a template names, like "Linear" or "Jira Cloud", in the template's
 * order, each once. A template's app name matches when it equals one of an entry's app names, or
 * starts with one followed by a space, unless the entry sets `exactApps`.
 */
export function suggestConnectors(appNames: string[], catalog: CatalogEntry[]): CatalogEntry[] {
  const found: CatalogEntry[] = [];
  for (const raw of appNames) {
    const name = normalize(raw);
    if (!name) continue;
    const entry = catalog.find((e) => e.apps.some((app) => name === app || (!e.exactApps && name.startsWith(`${app} `))));
    if (entry && !found.includes(entry)) found.push(entry);
  }
  return found;
}

/** Catalog entries whose name, description, or app names contain every word of `query`. */
export function filterCatalog(catalog: CatalogEntry[], query: string): CatalogEntry[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  return catalog.filter((e) => {
    const text = [e.name, e.description, ...e.apps].join(" ").toLowerCase();
    return words.every((w) => text.includes(w));
  });
}

/** The line under a connector's name. `names` maps familiar ids to names. */
export function connectorStatus(connector: Connector, names: Record<string, string>): string {
  if (!connector.auth) return "Not signed in";
  const on = connector.agents.map((id) => names[id]).filter(Boolean);
  const signed = connector.auth === "token" ? "Token saved" : "Signed in";
  return on.length ? `${signed}, on for ${on.join(", ")}` : `${signed}, not on for any familiar yet`;
}

/** The added connector for a catalog entry, whether it came from the catalog or was added by its URL. */
export function addedFor(entry: CatalogEntry, added: Connector[]): Connector | undefined {
  const same = (a: string, b: string) => {
    try {
      return new URL(a).href === new URL(b).href;
    } catch {
      return false;
    }
  };
  return added.find((c) => c.catalogId === entry.id || same(c.url, entry.url));
}

/** The connector ids a familiar should have on after turning `id` on or off. */
export function toggleId(ids: string[], id: string, on: boolean): string[] {
  return on ? (ids.includes(id) ? ids : [...ids, id]) : ids.filter((x) => x !== id);
}
