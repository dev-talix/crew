import { createMemo, createSignal, For, Show } from "solid-js";
import BotAvatar, { type Mood } from "./BotAvatar";
import { PROVIDER_NAMES, relativeTime, type Agent, type Coven, type Routine, type Workflow } from "./api";
import "./home.css";

import { arc, arcBetween, covenSectors, curve, NODE_RING, OUTER_RING, RUNES, seat, spellLinks, type Link } from "./circle";
import { ritualAgents } from "./spells";
import type { JSX } from "@solidjs/web";

export default function HomeCircle(props: {
  agents: Agent[];
  spells: Workflow[];
  routines: Routine[];
  busy: Record<string, boolean>;
  mood: (agentId: string) => Mood;
  now: number;
  /** When given, arcs mark each coven's run of seats; familiars should come in coven order. */
  covens?: Coven[];
  /** Shown in the middle instead of the familiar count, like a coven's name. */
  title?: string;
  /** Shown in the middle under the status, like a button to add familiars. */
  centerAction?: JSX.Element;
  onOpenAgent: (id: string) => void;
  onOpenSpells: () => void;
}) {
  const [hoverSpell, setHoverSpell] = createSignal<string | null>(null);
  const [hoverAgent, setHoverAgent] = createSignal<string | null>(null);
  const count = () => props.agents.length;
  const index = (id: string) => props.agents.findIndex((a) => a.id === id);
  const seatOf = (id: string) => seat(index(id), count());
  const links = createMemo(() => spellLinks(props.spells, props.agents.map((a) => a.id)));
  const lit = (link: Link) => hoverSpell() === link.spell.id || (hoverAgent() !== null && (link.from === hoverAgent() || link.to === hoverAgent()));
  const working = () => props.agents.filter((a) => props.busy[a.id]);
  // A ritual that casts a spell marks the familiars its spell starts with.
  const reaches = (r: Routine) => ritualAgents(r, props.spells);
  const rituals = (id: string) => props.routines.filter((r) => r.enabled && r.nextRunAt && reaches(r).includes(id));
  const nextRitual = () =>
    props.routines
      .filter((r) => r.enabled && r.nextRunAt && reaches(r).some((id) => index(id) >= 0))
      .sort((a, b) => a.nextRunAt! - b.nextRunAt!)[0];
  const small = () => count() > 8;
  const sectors = createMemo(() => (props.covens?.length ? covenSectors(props.agents.map((a) => a.covenId ?? null)) : []));
  const covenName = (id: string) => props.covens?.find((c) => c.id === id)?.name ?? "";
  const SECTOR_RING = OUTER_RING + 2.6;
  const LABEL_RING = OUTER_RING + 6.5;

  return (
    <div class={["home-circle", { small: small() }]}>
      <svg class="home-circle-art" viewBox="0 0 100 100" aria-hidden="true">
        <circle class="ring-outer" cx="50" cy="50" r={OUTER_RING} />
        <circle class="ring-seats" cx="50" cy="50" r={NODE_RING} />
        <circle class="ring-inner" cx="50" cy="50" r="17" />
        <For each={Array.from({ length: RUNES }, (_, i) => i)}>
          {(i) => {
            const a = (i * 2 * Math.PI) / RUNES;
            const [inner, outer] = i % 3 === 0 ? [OUTER_RING - 2.4, OUTER_RING] : [OUTER_RING - 1.2, OUTER_RING];
            return <line class="rune" x1={50 + inner * Math.cos(a)} y1={50 + inner * Math.sin(a)} x2={50 + outer * Math.cos(a)} y2={50 + outer * Math.sin(a)} />;
          }}
        </For>
        <g class="sigil">
          <rect x="43.5" y="43.5" width="13" height="13" />
          <rect x="43.5" y="43.5" width="13" height="13" transform="rotate(45 50 50)" />
        </g>
        <For each={sectors()}>{(sector) => <path class="coven-arc" d={arcBetween(sector.start, sector.end, SECTOR_RING)} />}</For>
        <For each={props.agents}>
          {(agent, i) => (
            <Show when={rituals(agent.id).length > 0}>
              {(() => {
                const center = -90 + (360 * i()) / count();
                const list = rituals(agent.id);
                const next = Math.min(...list.map((r) => r.nextRunAt!));
                return (
                  <path class="ritual-arc" d={arc(center, 9)}>
                    <title>
                      {agent.name}: {list.length} {list.length === 1 ? "ritual" : "rituals"}, next {relativeTime(next, props.now)}
                    </title>
                  </path>
                );
              })()}
            </Show>
          )}
        </For>
        <For each={working()}>
          {(agent) => {
            const p = () => seatOf(agent.id);
            return <line class="channel" x1="50" y1="50" x2={p().x} y2={p().y} style={{ stroke: agent.avatar.color }} />;
          }}
        </For>
        <For each={links()}>
          {(link) => {
            const shape = () => curve(seatOf(link.from), seatOf(link.to), link.lane);
            return (
              <g class={["spell-link", { lit: lit(link), check: link.kind === "check" }]}>
                <path class="spell-line" d={shape().d} />
                <path
                  class="spell-hit"
                  d={shape().d}
                  onMouseEnter={() => setHoverSpell(link.spell.id)}
                  onMouseLeave={() => setHoverSpell(null)}
                  onClick={props.onOpenSpells}
                />
              </g>
            );
          }}
        </For>
      </svg>

      <For each={links()}>
        {(link) => (
          <Show when={hoverSpell() === link.spell.id}>
            <span
              class="spell-label"
              style={{ left: `${curve(seatOf(link.from), seatOf(link.to), link.lane).label.x}%`, top: `${curve(seatOf(link.from), seatOf(link.to), link.lane).label.y}%` }}
            >
              {link.spell.name}
            </span>
          </Show>
        )}
      </For>

      <For each={sectors()}>
        {(sector) => {
          const a = (sector.mid * Math.PI) / 180;
          return (
            <span class="coven-label" style={{ left: `${50 + LABEL_RING * Math.cos(a)}%`, top: `${50 + LABEL_RING * Math.sin(a)}%` }}>
              {covenName(sector.covenId)}
            </span>
          );
        }}
      </For>

      <div class="circle-center">
        <span class="circle-count">{props.title ?? `${count()} ${count() === 1 ? "familiar" : "familiars"}`}</span>
        <span class="circle-status">
          {working().length
            ? `${working().map((a) => a.name).join(", ")} ${working().length === 1 ? "is" : "are"} working`
            : props.title
              ? count()
                ? `${count()} ${count() === 1 ? "familiar" : "familiars"}, all resting`
                : "No familiars yet"
              : "All resting"}
        </span>
        <Show when={nextRitual()}>
          {(r) => <span class="circle-next">Next ritual {relativeTime(r().nextRunAt!, props.now)}</span>}
        </Show>
        <Show when={props.centerAction}>
          <span class="circle-action">{props.centerAction}</span>
        </Show>
      </div>

      <For each={props.agents}>
        {(agent, i) => {
          const p = () => seat(i(), count());
          return (
            <button
              class={["circle-familiar", { working: !!props.busy[agent.id], dim: hoverAgent() !== null && hoverAgent() !== agent.id }]}
              style={{ left: `${p().x}%`, top: `${p().y}%`, "--agent": agent.avatar.color }}
              title={`Open ${agent.name}`}
              onMouseEnter={() => setHoverAgent(agent.id)}
              onMouseLeave={() => setHoverAgent(null)}
              onFocus={() => setHoverAgent(agent.id)}
              onBlur={() => setHoverAgent(null)}
              onClick={() => props.onOpenAgent(agent.id)}
            >
              <span class="circle-avatar">
                <BotAvatar avatar={agent.avatar} size={small() ? 34 : 44} mood={props.mood(agent.id)} />
              </span>
              <span class="circle-name">{agent.name}</span>
              <span class="circle-sub">{props.busy[agent.id] ? "Working" : PROVIDER_NAMES[agent.provider]}</span>
            </button>
          );
        }}
      </For>
    </div>
  );
}
