import { describe, expect, test } from "bun:test";
import { addedFor, connectorStatus, filterCatalog, suggestConnectors, toggleId, type CatalogEntry, type Connector } from "./connectors";

const entry = (id: string, apps: string[]): CatalogEntry => ({ id, name: id, url: `https://mcp.${id}.com/mcp`, description: "", apps, tokenHelp: null });
const catalog = [entry("linear", ["linear"]), entry("atlassian", ["atlassian", "jira", "confluence"]), entry("monday", ["monday", "monday.com"])];

describe("suggestConnectors", () => {
  test("matches template app names to catalog entries, once each, in order", () => {
    const found = suggestConnectors(["Jira Cloud", "Gmail", "linear", "Confluence", "Monday.com", ""], catalog);
    expect(found.map((e) => e.id)).toEqual(["atlassian", "linear", "monday"]);
  });

  test("ignores names that only contain an app name", () => {
    expect(suggestConnectors(["Nonlinear notes", "Mondays"], catalog)).toEqual([]);
  });
});

test("filterCatalog matches every word against the name, description, and app names", () => {
  const described = [...catalog, { ...entry("granola", ["granola"]), description: "Meeting notes and transcripts" }];
  expect(filterCatalog(described, "").length).toBe(4);
  expect(filterCatalog(described, "JIRA").map((e) => e.id)).toEqual(["atlassian"]);
  expect(filterCatalog(described, " meeting  notes ").map((e) => e.id)).toEqual(["granola"]);
  expect(filterCatalog(described, "meeting jira")).toEqual([]);
});

describe("connectorStatus", () => {
  const base: Connector = { id: "c1", name: "Linear", url: "", catalogId: "linear", key: "linear", auth: null, agents: [] };

  test("says whether it's signed in and who has it on", () => {
    expect(connectorStatus(base, {})).toBe("Not signed in");
    expect(connectorStatus({ ...base, auth: "oauth" }, {})).toBe("Signed in, not on for any familiar yet");
    expect(connectorStatus({ ...base, auth: "token", agents: ["a", "gone"] }, { a: "Scribe" })).toBe("Token saved, on for Scribe");
  });
});

test("toggleId adds and removes without duplicates", () => {
  expect(toggleId(["a"], "b", true)).toEqual(["a", "b"]);
  expect(toggleId(["a"], "a", true)).toEqual(["a"]);
  expect(toggleId(["a", "b"], "a", false)).toEqual(["b"]);
});

test("addedFor finds a catalog entry added from the catalog or by its URL", () => {
  const stripe = { ...entry("stripe", ["stripe"]), url: "https://mcp.stripe.com" };
  const byUrl: Connector = { id: "c9", name: "stripe", url: "https://mcp.stripe.com/", catalogId: null, key: "stripe", auth: null, agents: [] };
  expect(addedFor(stripe, [byUrl])?.id).toBe("c9");
  expect(addedFor(catalog[0], [{ ...byUrl, id: "c1", catalogId: "linear", url: "https://other.example/mcp" }])?.id).toBe("c1");
  expect(addedFor(catalog[0], [byUrl])).toBeUndefined();
});
