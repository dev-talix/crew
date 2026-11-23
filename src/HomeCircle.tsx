import { createMemo, createSignal, For, Show } from "solid-js";
import BotAvatar, { type Mood } from "./BotAvatar";
import { PROVIDER_NAMES, relativeTime, type Agent, type Routine, type Workflow } from "./api";
import "./home.css";

import { arc, curve, NODE_RING, OUTER_RING, RUNES, seat, spellLinks, type Link } from "./circle";

export default function HomeCircle(props: {
  agents: Agent[];
  spells: Workflow[];
  routines: Routine[];
  busy: Record<string, boolean>;
  mood: (agentId: string) => Mood;
  now: number;
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
  const rituals = (id: string) => props.routines.filter((r) => r.agentId === id && r.enabled && r.nextRunAt);
  const nextRitual = () =>
    props.routines.filter((r) => r.enabled && r.nextRunAt && index(r.agentId) >= 0).sort((a, b) => a.nextRunAt! - b.nextRunAt!)[0];
  const small = () => count() > 8;

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
              <g class={["spell-link", { lit: lit(link) }]}>
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

      <div class="circle-center">
        <span class="circle-count">
          {count()} {count() === 1 ? "familiar" : "familiars"}
        </span>
        <span class="circle-status">
          {working().length ? `${working().map((a) => a.name).join(", ")} ${working().length === 1 ? "is" : "are"} working` : "All resting"}
        </span>
        <Show when={nextRitual()}>
          {(r) => <span class="circle-next">Next ritual {relativeTime(r().nextRunAt!, props.now)}</span>}
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
