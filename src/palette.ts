/** Fuzzy matching and ranking for the ⌘K palette. */

export interface Match {
  score: number;
  /** Positions in the label that matched, for highlighting. */
  indices: number[];
}

export interface PaletteItem {
  id: string;
  label: string;
  /** Other words the item answers to, like "keychain" for the vault. Matched without highlighting. */
  keywords?: string;
}

export interface Ranked<T extends PaletteItem> {
  item: T;
  indices: number[];
}

const isWordStart = (text: string, i: number) => i === 0 || /[\s\-_/:.,(]/.test(text[i - 1]);

/**
 * Scores `query` against `text`, case-insensitively. A run of the query inside the text beats letters
 * spread across it, and matches at the start of the text or of a word score higher. Null means no match.
 */
export function fuzzyMatch(query: string, text: string): Match | null {
  const q = query.trim().toLowerCase();
  const t = text.toLowerCase();
  if (!q) return { score: 0, indices: [] };

  const run = t.indexOf(q);
  if (run >= 0) {
    // Prefer a run that starts a word, even if an earlier one sits mid-word ("set" in "Reset settings").
    let at = run;
    for (let i = run; i >= 0; i = t.indexOf(q, i + 1)) {
      if (isWordStart(t, i)) {
        at = i;
        break;
      }
    }
    const score = 1000 + (at === 0 ? 400 : isWordStart(t, at) ? 200 : 0) - at - (t.length - q.length) * 0.5;
    return { score, indices: Array.from({ length: q.length }, (_, k) => at + k) };
  }

  // Letters in order, like "nsp" for "New spell": first trying to land each letter on a word start.
  return inOrder(q, t, true) ?? inOrder(q, t, false);
}

function inOrder(q: string, t: string, wordStarts: boolean): Match | null {
  const indices: number[] = [];
  let score = 0;
  let from = 0;
  for (const ch of q.replace(/\s+/g, "")) {
    let found = t.indexOf(ch, from);
    if (wordStarts) {
      for (let i = found; i >= 0 && !isWordStart(t, i); i = t.indexOf(ch, i + 1)) found = t.indexOf(ch, i + 1);
    }
    if (found < 0) return null;
    const last = indices[indices.length - 1];
    // Word starts and adjacent letters earn points; gaps cost them.
    score += isWordStart(t, found) ? 30 : last === found - 1 ? 15 : 0;
    score -= last === undefined ? found : found - last - 1;
    indices.push(found);
    from = found + 1;
  }
  return { score, indices };
}

/** Items that match `query`, best first; ties keep their original order. An empty query keeps them all. */
export function rank<T extends PaletteItem>(items: readonly T[], query: string): Ranked<T>[] {
  const scored: { item: T; indices: number[]; score: number; order: number }[] = [];
  items.forEach((item, order) => {
    const label = fuzzyMatch(query, item.label);
    const keyword = item.keywords ? fuzzyMatch(query, item.keywords) : null;
    // A keyword hit counts for less than the same hit on the name.
    const keywordScore = keyword ? keyword.score - 300 : -Infinity;
    if (label && label.score >= keywordScore) scored.push({ item, indices: label.indices, score: label.score, order });
    else if (keyword) scored.push({ item, indices: [], score: keywordScore, order });
  });
  scored.sort((a, b) => b.score - a.score || a.order - b.order);
  return scored.map(({ item, indices }) => ({ item, indices }));
}

/** Splits text into plain and highlighted parts, from matched positions. */
export function highlightParts(text: string, indices: readonly number[]): { text: string; hit: boolean }[] {
  const hits = new Set(indices);
  const parts: { text: string; hit: boolean }[] = [];
  for (let i = 0; i < text.length; i++) {
    const hit = hits.has(i);
    const last = parts[parts.length - 1];
    if (last && last.hit === hit) last.text += text[i];
    else parts.push({ text: text[i], hit });
  }
  return parts;
}

/** Splits a search snippet into parts, where the backend marks each match with \u0002 and \u0003. */
export function snippetParts(snippet: string): { text: string; hit: boolean }[] {
  const parts: { text: string; hit: boolean }[] = [];
  for (const [i, piece] of snippet.split(/[\u0002\u0003]/).entries()) {
    if (piece) parts.push({ text: piece.replace(/\s+/g, " "), hit: i % 2 === 1 });
  }
  return parts;
}
