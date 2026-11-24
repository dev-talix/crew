import { relativeTime, type Agent, type Peer } from "./api";

/** The parts of a linked familiar's id, `peer:<peerId>:<agentId>`, or null for one on this Mac. */
export function parseRemoteId(id: string): { peerId: string; agentId: string } | null {
  const match = /^peer:([^:]+):(.+)$/.exec(id);
  return match ? { peerId: match[1], agentId: match[2] } : null;
}

/** Lives on a linked Mac, so only that Mac can change it. Any linked Mac can delete it. */
export const isRemote = (agent: Pick<Agent, "home">) => !!agent.home;

/**
 * Whether a `link_reload` event means the open chat must load again. `agentId` is a linked
 * familiar's id, or null for every familiar from linked Macs.
 */
export function reloadsOpenChat(agentId: string | null, open: Pick<Agent, "id" | "home"> | null | undefined): boolean {
  if (!open) return false;
  return agentId === null ? isRemote(open) : agentId === open.id;
}

/**
 * A side conversation row in a linked familiar's chat. The conversation lives on that familiar's
 * Mac, so the row names the Mac instead of opening it. Null for a familiar on this Mac.
 */
export function remoteSideLabel(label: string, agent: Pick<Agent, "home">): string | null {
  return agent.home ? `${label} (open it on ${agent.home.name})` : null;
}

/**
 * Whether a side row's source points at a conversation on a linked Mac (`peer:<peerId>:<threadId>`),
 * which only that Mac can open.
 */
export const isRemoteThread = (source: string | null) => !!source && parseRemoteId(source) !== null;

/** Lives on a linked Mac that can't be reached right now. */
export const homeOffline = (agent: Pick<Agent, "home">) => !!agent.home && !agent.home.online;

/** What the chat says, in place of the composer, while a linked familiar's Mac is offline. */
export function offlineNotice(agent: Pick<Agent, "name" | "home">): string {
  const mac = agent.home?.name ?? "Its Mac";
  return `${mac} is offline, so ${agent.name} can't be reached right now. Try again when that Mac is back.`;
}

/** What the confirm sheet says before a linked familiar is deleted on its Mac. */
export function remoteDeleteNotice(agent: Pick<Agent, "name" | "home">): string {
  const mac = agent.home?.name ?? "its Mac";
  return `${agent.name} lives on ${mac}. Deleting it there removes its chats, memory, and settings on every linked Mac. Its workspace folder on ${mac} stays.`;
}

/** What the badge on a linked familiar says when you hover it. */
export function homeTitle(agent: Pick<Agent, "name" | "home">): string {
  const mac = agent.home?.name ?? "another Mac";
  if (agent.home && !agent.home.online) return `${mac} can't be reached right now.`;
  return `${agent.name} lives on ${mac}. Open Grimoire there to change it.`;
}

export interface HomeGroup<T> {
  /** Null for this Mac. */
  peerId: string | null;
  name: string | null;
  online: boolean;
  agents: T[];
}

/** This Mac's familiars first, then one group per linked Mac, sorted by the Mac's name. */
export function groupByHome<T extends Pick<Agent, "home">>(agents: T[]): HomeGroup<T>[] {
  const local: HomeGroup<T> = { peerId: null, name: null, online: true, agents: [] };
  const remote = new Map<string, HomeGroup<T>>();
  for (const agent of agents) {
    const home = agent.home;
    if (!home) {
      local.agents.push(agent);
      continue;
    }
    let group = remote.get(home.peerId);
    if (!group) remote.set(home.peerId, (group = { peerId: home.peerId, name: home.name, online: home.online, agents: [] }));
    group.agents.push(agent);
  }
  const macs = [...remote.values()].sort((a, b) => a.name!.localeCompare(b.name!));
  return [local, ...macs].filter((g) => g.agents.length > 0);
}

/** A linked Mac's state in words, so it never rests on the dot's color alone. */
export function peerState(peer: Pick<Peer, "online" | "lastSeen">, now = Date.now()): string {
  if (peer.online) return "Online";
  return peer.lastSeen ? `Offline, last seen ${relativeTime(peer.lastSeen, now)}` : "Offline";
}

/** Time left on a pairing code, like "4:05", or null once it has expired. */
export function countdown(expiresAt: number, now = Date.now()): string | null {
  const left = Math.ceil((expiresAt - now) / 1000);
  if (left <= 0) return null;
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
}

export interface RunsOnOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/** "Runs on" choices for a new familiar: this Mac, each linked Mac, then SSH. */
export function runsOnOptions(peers: Pick<Peer, "id" | "name" | "online">[]): RunsOnOption[] {
  return [
    { value: "here", label: "This Mac" },
    ...peers.map((p) => ({ value: `peer:${p.id}`, label: p.online ? p.name : `${p.name} (offline)`, disabled: !p.online })),
    { value: "ssh", label: "Another Mac over SSH" },
  ];
}
