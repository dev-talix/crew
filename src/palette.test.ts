import { describe, expect, test } from "bun:test";
import { fuzzyMatch, highlightParts, rank, snippetParts } from "./palette";

const items = [
  { id: "home", label: "Home" },
  { id: "rituals", label: "Rituals" },
  { id: "a1", label: "Researcher" },
  { id: "a2", label: "Scribe" },
  { id: "spell", label: "Topic brief" },
  { id: "new-spell", label: "New spell" },
  { id: "summon", label: "Summon a familiar", keywords: "create add agent" },
  { id: "vault", label: "Settings: Vault", keywords: "keychain passwords secrets" },
  { id: "usage", label: "Settings: Usage", keywords: "tokens limits" },
];
const ids = (query: string) => rank(items, query).map((r) => r.item.id);

describe("palette ranking", () => {
  test("an empty query keeps every item in order", () => {
    expect(ids("")).toEqual(items.map((i) => i.id));
  });

  test("a prefix beats a word start, which beats a match mid-word", () => {
    // Shorter names win among prefixes; "New spell" starts a word; Researcher and Rituals have it mid-word.
    expect(ids("s")).toEqual(["a2", "vault", "usage", "summon", "new-spell", "a1", "rituals"]);
    expect(ids("sc")[0]).toBe("a2");
    expect(ids("re")[0]).toBe("a1");
  });

  test("letters in order match across words", () => {
    expect(ids("nsp")[0]).toBe("new-spell");
    expect(ids("rsch")).toEqual(["a1"]);
    expect(ids("xyz")).toEqual([]);
  });

  test("keywords find items, below a hit on a name", () => {
    expect(ids("keychain")).toEqual(["vault"]);
    expect(ids("agent")).toEqual(["summon"]);
    expect(ids("us")[0]).toBe("usage");
  });

  test("highlights the matched letters", () => {
    const [top] = rank(items, "vau");
    expect(top.item.id).toBe("vault");
    expect(highlightParts(top.item.label, top.indices)).toEqual([
      { text: "Settings: ", hit: false },
      { text: "Vau", hit: true },
      { text: "lt", hit: false },
    ]);
  });
});

describe("fuzzy match", () => {
  test("prefers a run that starts a word", () => {
    expect(fuzzyMatch("set", "Reset settings")?.indices).toEqual([6, 7, 8]);
  });

  test("falls back to any letters when word starts don't fit", () => {
    expect(fuzzyMatch("ae", "age bar")?.indices).toEqual([0, 2]);
  });
});

test("splits search snippets on the match markers", () => {
  expect(snippetParts("…the \u0002store\u0003 setters\n now \u0002store\u0003")).toEqual([
    { text: "…the ", hit: false },
    { text: "store", hit: true },
    { text: " setters now ", hit: false },
    { text: "store", hit: true },
  ]);
});
