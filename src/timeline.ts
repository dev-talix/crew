import type { Message } from "./api";

export type ToolKind = "command" | "edit" | "read" | "search" | "web" | "tool";

export interface ToolEntry {
  kind: ToolKind;
  label: string;
  count: number;
}

export function workspaceRelative(text: string, workspace: string): string {
  return text.replaceAll(`${workspace}/`, "");
}

export function classifyTool(raw: string): ToolEntry {
  const text = raw.trim();
  if (text.startsWith("$ ")) return { kind: "command", label: `Ran ${text.slice(2)}`, count: 1 };
  const edit = /^(Edit|Write|NotebookEdit) (.+)$/.exec(text);
  if (edit) {
    const files = edit[2].split(", ").filter(Boolean);
    return { kind: "edit", label: `Edited ${edit[2]}`, count: Math.max(files.length, 1) };
  }
  if (/^Read /.test(text)) return { kind: "read", label: text, count: 1 };
  const search = /^(Grep|Glob) (.+)$/.exec(text);
  if (search) return { kind: "search", label: `Searched for ${search[2]}`, count: 1 };
  if (/^(Fetch|Search) |^Web search/.test(text)) return { kind: "web", label: text, count: 1 };
  return { kind: "tool", label: `Used ${text}`, count: 1 };
}

const PHRASES: Record<ToolKind, (n: number) => string> = {
  command: (n) => `ran ${n} ${n === 1 ? "command" : "commands"}`,
  edit: (n) => `edited ${n} ${n === 1 ? "file" : "files"}`,
  read: (n) => `read ${n} ${n === 1 ? "file" : "files"}`,
  search: (n) => `ran ${n} ${n === 1 ? "search" : "searches"}`,
  web: (n) => `looked up ${n} ${n === 1 ? "page" : "pages"}`,
  tool: (n) => `used ${n} ${n === 1 ? "tool" : "tools"}`,
};

const ORDER: ToolKind[] = ["command", "edit", "read", "search", "web", "tool"];

export function summarizeTools(entries: ToolEntry[]): string {
  if (entries.length === 1) return entries[0].label;
  const totals = new Map<ToolKind, number>();
  for (const entry of entries) totals.set(entry.kind, (totals.get(entry.kind) ?? 0) + entry.count);
  const parts = ORDER.filter((kind) => totals.has(kind)).map((kind) => PHRASES[kind](totals.get(kind)!));
  const sentence = parts.length > 1 ? `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}` : parts[0];
  return sentence.charAt(0).toUpperCase() + sentence.slice(1);
}

export function dominantKind(entries: ToolEntry[]): ToolKind {
  const totals = new Map<ToolKind, number>();
  for (const entry of entries) totals.set(entry.kind, (totals.get(entry.kind) ?? 0) + 1);
  return [...totals.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] ?? "tool";
}

export type Row =
  | { kind: "user"; key: string; message: Message }
  | { kind: "divider"; key: string; message: Message }
  | { kind: "error"; key: string; message: Message }
  | { kind: "assistant"; key: string; message: Message }
  | { kind: "tools"; key: string; messages: Message[] };

export function buildRows(messages: readonly Message[], previous: Map<string, Row>): Row[] {
  const rows: Row[] = [];
  let tools: Message[] = [];
  const reuse = (row: Row): Row => {
    const old = previous.get(row.key);
    if (old && old.kind === row.kind) {
      if (row.kind !== "tools") return old;
      if (old.kind === "tools" && old.messages.length === row.messages.length) return old;
    }
    return row;
  };
  const flushTools = () => {
    if (!tools.length) return;
    rows.push(reuse({ kind: "tools", key: `tools:${tools[0].id}`, messages: tools }));
    tools = [];
  };
  for (const message of messages) {
    if (message.role === "tool") {
      tools.push(message);
      continue;
    }
    flushTools();
    rows.push(reuse({ kind: message.role, key: `${message.role}:${message.id}`, message } as Row));
  }
  flushTools();
  return rows;
}

export interface TurnInfo {
  startedAt: number;
  endsTurn: boolean;
}

export function turnInfo(messages: readonly Message[]): Map<number, TurnInfo> {
  const info = new Map<number, TurnInfo>();
  let startedAt = 0;
  let lastAgentId: number | null = null;
  const close = () => {
    if (lastAgentId !== null) info.set(lastAgentId, { startedAt, endsTurn: true });
    lastAgentId = null;
  };
  for (const message of messages) {
    if (message.role === "user" || message.role === "divider") {
      close();
      startedAt = message.createdAt;
    } else if (message.role === "assistant") {
      if (lastAgentId !== null) info.set(lastAgentId, { startedAt, endsTurn: false });
      lastAgentId = message.id;
    }
  }
  close();
  return info;
}

export function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (minutes < 60) return rest ? `${minutes}m ${rest}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`;
}
