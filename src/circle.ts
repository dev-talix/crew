// Geometry for the summoning circle on Home, kept apart from the component so it can be tested.
import type { Workflow } from "./api";
import { handoffs, type Handoff } from "./spells";

export const NODE_RING = 33; // where familiars sit, in percent of the circle's width
export const OUTER_RING = 47;
export const RUNES = 36;

type Point = { x: number; y: number };

/** Evenly around the ring, starting at the top. */
export function seat(index: number, count: number, radius = NODE_RING): Point {
  const angle = -Math.PI / 2 + (2 * Math.PI * index) / Math.max(count, 1);
  return { x: 50 + radius * Math.cos(angle), y: 50 + radius * Math.sin(angle) };
}

export interface Link {
  spell: Workflow;
  from: string;
  to: string;
  /** Which of several links between the same two familiars this is, for spacing the curves. */
  lane: number;
  /** "check" when only a check sends work this way, like a reviewer sending a draft back. */
  kind: Handoff["kind"];
}

/**
 * One link per pair of different familiars a spell hands work between: from each group of steps to the
 * next, which fans out to and joins from steps that run alongside each other, and along each check.
 */
export function spellLinks(spells: Workflow[], agentIds: string[]): Link[] {
  const lanes = new Map<string, number>();
  const links: Link[] = [];
  for (const spell of spells) {
    const seen = new Set<string>();
    for (const { from, to, kind } of handoffs(spell.steps)) {
      if (from === to || !agentIds.includes(from) || !agentIds.includes(to)) continue;
      const pair = [from, to].sort().join(":");
      if (seen.has(pair)) continue;
      seen.add(pair);
      const lane = lanes.get(pair) ?? 0;
      lanes.set(pair, lane + 1);
      links.push({ spell, from, to, lane, kind });
    }
  }
  return links;
}

/** A curve between two seats that bows toward the middle, a little more for each extra lane. */
export function curve(a: Point, b: Point, lane: number) {
  const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  const pull = 0.3 + lane * 0.18;
  const control = { x: mid.x + (50 - mid.x) * pull, y: mid.y + (50 - mid.y) * pull };
  const label = { x: (a.x + 2 * control.x + b.x) / 4, y: (a.y + 2 * control.y + b.y) / 4 };
  return { d: `M${a.x} ${a.y}Q${control.x} ${control.y} ${b.x} ${b.y}`, label };
}

export function arc(center: number, spread: number, radius = OUTER_RING) {
  const point = (deg: number) => {
    const a = (deg * Math.PI) / 180;
    return `${50 + radius * Math.cos(a)} ${50 + radius * Math.sin(a)}`;
  };
  return `M${point(center - spread)}A${radius} ${radius} 0 0 1 ${point(center + spread)}`;
}


export interface Sector {
  covenId: string;
  /** Degrees, clockwise from the right, like the seats. */
  start: number;
  end: number;
  mid: number;
}

/** One arc per run of neighbors in the same coven, for seats in the order they sit. */
export function covenSectors(covenIds: (string | null)[]): Sector[] {
  const count = covenIds.length;
  const step = 360 / Math.max(count, 1);
  const gap = Math.min(4, step * 0.15);
  const sectors: Sector[] = [];
  let i = 0;
  while (i < count) {
    const id = covenIds[i];
    let j = i;
    while (j + 1 < count && covenIds[j + 1] === id) j++;
    if (id) {
      const start = -90 + step * i - step / 2 + gap;
      const end = -90 + step * j + step / 2 - gap;
      sectors.push({ covenId: id, start, end, mid: (start + end) / 2 });
    }
    i = j + 1;
  }
  return sectors;
}

/** An SVG arc from one angle to another, the long way round when it spans more than half. */
export function arcBetween(start: number, end: number, radius: number) {
  const point = (deg: number) => {
    const a = (deg * Math.PI) / 180;
    return `${50 + radius * Math.cos(a)} ${50 + radius * Math.sin(a)}`;
  };
  // A lone coven filling the whole ring can't be one arc, so it stops just short of a full turn.
  const span = Math.min(end - start, 359.5);
  return `M${point(start)}A${radius} ${radius} 0 ${span > 180 ? 1 : 0} 1 ${point(start + span)}`;
}
