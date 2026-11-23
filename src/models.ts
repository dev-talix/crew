import { api, type Agent, type ModelOption, type Provider } from "./api";

const catalogs = new Map<Provider, Promise<ModelOption[]>>();

export function modelCatalog(provider: Provider): Promise<ModelOption[]> {
  let catalog = catalogs.get(provider);
  if (!catalog) {
    catalog = api.listModels(provider);
    catalog.catch(() => catalogs.delete(provider));
    catalogs.set(provider, catalog);
  }
  return catalog;
}

const EFFORT_NAMES: Record<string, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
  ultra: "Ultra",
};

export const effortName = (effort: string) => EFFORT_NAMES[effort] ?? effort;

export function modelName(id: string, catalog: ModelOption[] = []): string {
  const known = catalog.find((m) => m.id === id);
  if (known?.name) return known.name;
  const claude = /^claude-([a-z]+)-(\d+)-(\d+)/.exec(id);
  if (claude) return `${claude[1][0].toUpperCase()}${claude[1].slice(1)} ${claude[2]}.${claude[3]}`;
  if (/^gpt-/.test(id)) return id.split("-").map((part, i) => (i === 0 ? "GPT" : part.charAt(0).toUpperCase() + part.slice(1))).join("-");
  if (/^[a-z]+$/.test(id)) return id[0].toUpperCase() + id.slice(1);
  return id;
}

export function runningModelName(agent: Agent, catalog: ModelOption[] = []): string {
  if (agent.model) {
    const resolved = agent.activeModel?.includes(agent.model) ? agent.activeModel : agent.model;
    return modelName(resolved, catalog);
  }
  if (agent.activeModel) return modelName(agent.activeModel, catalog);
  const fallback = catalog.find((m) => m.isDefault);
  return fallback ? fallback.name : "Default model";
}
