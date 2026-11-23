import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { createEffect, createSignal, onSettled, type Accessor } from "solid-js";
import type { Avatar } from "./avatar";
import type { ThemeFile } from "./theme";

export type Provider = "codex" | "claude";
export type Access = "workspace" | "full";

export const PROVIDER_NAMES: Record<Provider, string> = { codex: "Codex", claude: "Claude" };

export interface Agent {
  id: string;
  name: string;
  provider: Provider;
  model: string | null;
  workspace: string;
  access: Access;
  sessionId: string | null;
  avatar: Avatar;
  effort: string | null;
  activeModel: string | null;
  /** The coven it sits in, if any. */
  covenId: string | null;
  createdAt: number;
}

export interface Coven {
  id: string;
  name: string;
  createdAt: number;
}

export interface ModelOption {
  id: string;
  name: string;
  description: string;
  efforts: string[];
  defaultEffort: string | null;
  isDefault: boolean;
}

export interface Message {
  id: number;
  agentId: string;
  role: "user" | "assistant" | "tool" | "error" | "divider";
  text: string;
  status: "done" | "streaming" | "queued" | "cancelled" | "stopped" | "interrupted";
  source: string | null;
  createdAt: number;
  updatedAt: number | null;
  /** Files and images sent with a user message. */
  attachments?: Attachment[];
}

export interface Attachment {
  id: number;
  messageId: number;
  name: string;
  kind: "image" | "file";
  size: number;
  path: string;
}

/** A file saved by `stage_attachment`, waiting for its message to be sent. */
export interface StagedFile {
  token: string;
  name: string;
  kind: "image" | "file";
  size: number;
}

/** Where the window loads a stored attachment's bytes, for thumbnails. */
export const attachmentUrl = (id: number) => convertFileSrc(String(id), "attachment");

export type Schedule = { kind: "interval"; minutes: number } | { kind: "daily"; time: string; days: number[] };

export interface Routine {
  id: string;
  agentId: string;
  name: string;
  prompt: string;
  schedule: Schedule;
  enabled: boolean;
  lastRunAt: number | null;
  nextRunAt: number | null;
  createdAt: number;
}

export interface RoutineInput {
  id?: string;
  agentId: string;
  name: string;
  prompt: string;
  schedule: Schedule;
  enabled: boolean;
}

export interface WorkflowStep {
  agentId: string;
  prompt: string;
}

export interface Workflow {
  id: string;
  name: string;
  steps: WorkflowStep[];
  createdAt: number;
  /** The agent that created it with its Grimoire tools. */
  createdBy: string | null;
}

export interface WorkflowInput {
  id?: string;
  name: string;
  steps: WorkflowStep[];
}

export interface WorkflowProgress {
  workflowId: string;
  step: number;
  total: number;
  agentId: string | null;
  state: "running" | "done" | "failed" | "stopped";
  error: string | null;
}

export interface ProviderStatus {
  provider: Provider;
  installed: boolean;
  version: string | null;
  signedIn: boolean | null;
  method: string | null;
  detail: string | null;
}

export type DockEdge = "off" | "left" | "right";

export interface Settings {
  claudeBilling: "subscription" | "api";
  dockEdge: DockEdge;
}

export interface NewAgentInput {
  name: string;
  provider: Provider;
  model: string;
  workspace: string;
  access: Access;
  avatar: Avatar;
  effort: string;
  covenId?: string | null;
}

export interface AgentChanges {
  name: string;
  model: string;
  access: Access;
  avatar: Avatar;
  effort: string;
  /** Left out keeps the coven; null unbinds. */
  covenId?: string | null;
}

export function changesFor(agent: Agent, patch: Partial<AgentChanges> = {}): AgentChanges {
  return {
    name: agent.name,
    model: agent.model ?? "",
    access: agent.access,
    avatar: agent.avatar,
    effort: agent.effort ?? "",
    ...patch,
  };
}

export const PAGE_SIZE = 100;

export interface ThemeEntry {
  id: string;
  builtIn: boolean;
  theme: ThemeFile;
}

export interface ThemeList {
  active: string;
  themes: ThemeEntry[];
  /** Files in the themes folder that could not be loaded. */
  problems: { file: string; error: string }[];
}

export type VaultKind = "login" | "api_key" | "card";

export interface VaultItem {
  id: string;
  kind: VaultKind;
  name: string;
  /** The website or service. */
  site: string | null;
  /** The username for a login, or the name on a card. */
  username: string | null;
  /** Safe to show, like the last digits of a card. */
  hint: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface VaultItemInput {
  id: string | null;
  kind: VaultKind;
  name: string;
  site: string | null;
  username: string | null;
  /** Secret fields to set. On an edit, fields left out keep their value. */
  secret: Record<string, string>;
}

export interface VaultActivity {
  uses: { agentId: string | null; field: string; outcome: "allowed" | "denied"; at: number }[];
  alwaysAllowed: string[];
}

export interface ApprovalRequest {
  id: string;
  agentId: string;
  kind: "vault" | "command" | "file";
  title: string;
  detail: string;
  canRemember: boolean;
}

export type ApprovalDecision = "once" | "always" | "deny";

export interface PlanLimits {
  /** Codex's `account/rateLimits/read` reply. */
  codex: Record<string, unknown> | null;
  codexError: string | null;
  /** Claude's latest `rate_limit_event` info and when it arrived. */
  claude: { info: Record<string, unknown>; at: number } | null;
}

export interface UpdateCheck {
  current: string;
  latest: { version: string; url: string; publishedAt: string | null } | null;
  newer: boolean;
}

export type PermissionMode = "off" | "rules" | "always";
export type RuleKind = "ask" | "allow";

export interface Permissions {
  mode: PermissionMode;
  rules: { kind: RuleKind; pattern: string }[];
}

export const api = {
  listAgents: () => invoke<Agent[]>("list_agents"),
  busyAgents: () => invoke<string[]>("busy_agents"),
  createAgent: (input: NewAgentInput) => invoke<Agent>("create_agent", { input }),
  updateAgent: (agentId: string, changes: AgentChanges) => invoke<Agent>("update_agent", { agentId, changes }),
  deleteAgent: (agentId: string) => invoke<void>("delete_agent", { agentId }),
  openWorkspace: (agentId: string) => invoke<void>("open_workspace", { agentId }),
  listMessages: (agentId: string, beforeId?: number) =>
    invoke<Message[]>("list_messages", { agentId, beforeId: beforeId ?? null, limit: PAGE_SIZE }),
  sendMessage: (agentId: string, text: string, attachments: string[] = []) =>
    invoke<Message>("send_message", { agentId, text, attachments }),
  stageAttachment: async (file: File) =>
    invoke<StagedFile>("stage_attachment", new Uint8Array(await file.arrayBuffer()), {
      headers: { "x-file-name": encodeURIComponent(file.name) },
    }),
  openAttachment: (id: number) => invoke<void>("open_attachment", { id }),
  stopTurn: (agentId: string) => invoke<void>("stop_turn", { agentId }),
  newChat: (agentId: string) => invoke<Message>("new_chat", { agentId }),
  lastMessages: () => invoke<Message[]>("last_messages"),
  listModels: (provider: Provider) => invoke<ModelOption[]>("list_models", { provider }),
  dockEdge: () => invoke<"left" | "right" | null>("dock_edge"),
  dockResize: (width: number, height: number, focus: boolean) => invoke<void>("dock_resize", { width, height, focus }),
  showMain: (agentId?: string) => invoke<void>("show_main", { agentId: agentId ?? null }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  providerStatus: () => invoke<ProviderStatus[]>("provider_status"),
  listRoutines: () => invoke<Routine[]>("list_routines"),
  saveRoutine: (input: RoutineInput) => invoke<Routine>("save_routine", { input }),
  deleteRoutine: (id: string) => invoke<void>("delete_routine", { id }),
  runRoutine: (id: string) => invoke<void>("run_routine", { id }),
  listWorkflows: () => invoke<Workflow[]>("list_workflows"),
  runningWorkflows: () => invoke<string[]>("running_workflows"),
  saveWorkflow: (input: WorkflowInput) => invoke<Workflow>("save_workflow", { input }),
  deleteWorkflow: (id: string) => invoke<void>("delete_workflow", { id }),
  runWorkflow: (id: string, input: string) => invoke<void>("run_workflow", { id, input }),
  stopWorkflow: (id: string) => invoke<void>("stop_workflow", { id }),
  listThemes: () => invoke<ThemeList>("list_themes"),
  saveTheme: (id: string | null, theme: ThemeFile) => invoke<ThemeEntry>("save_theme", { id, theme }),
  deleteTheme: (id: string) => invoke<void>("delete_theme", { id }),
  setTheme: (id: string) => invoke<void>("set_theme", { id }),
  openThemesFolder: () => invoke<void>("open_themes_folder"),
  setAppIcon: (icon: "paper" | "night") => invoke<void>("set_app_icon", { icon }),
  listCovens: () => invoke<Coven[]>("list_covens"),
  saveCoven: (id: string | null, name: string, members: string[]) => invoke<Coven>("save_coven", { id, name, members }),
  deleteCoven: (id: string) => invoke<void>("delete_coven", { id }),
  listVault: () => invoke<VaultItem[]>("list_vault"),
  vaultSecret: (id: string) => invoke<Record<string, string>>("vault_secret", { id }),
  saveVaultItem: (input: VaultItemInput) => invoke<VaultItem>("save_vault_item", { input }),
  deleteVaultItem: (id: string) => invoke<void>("delete_vault_item", { id }),
  vaultActivity: (id: string) => invoke<VaultActivity>("vault_activity", { id }),
  revokeVaultGrant: (id: string, agentId: string) => invoke<void>("revoke_vault_grant", { id, agentId }),
  answerApproval: (id: string, decision: ApprovalDecision) => invoke<void>("answer_approval", { id, decision }),
  waitingApprovals: () => invoke<ApprovalRequest[]>("waiting_approvals"),
  getPermissions: () => invoke<Permissions>("get_permissions"),
  usageSince: (since: number) => invoke<import("./usage").UsageRow[]>("usage_since", { since }),
  planLimits: () => invoke<PlanLimits>("plan_limits"),
  checkForUpdates: () => invoke<UpdateCheck>("check_for_updates"),
  savePermissions: (permissions: Permissions) => invoke<Permissions>("save_permissions", { permissions }),
};

/**
 * Runs async setup once the component has mounted. Unlisten functions passed to `keep` run when the
 * component is disposed, including ones that resolve after disposal.
 */
export function onMountAsync(setup: (keep: (stop: () => void) => void) => Promise<void>) {
  onSettled(() => {
    let disposed = false;
    const stops: (() => void)[] = [];
    void setup((stop) => (disposed ? stop() : void stops.push(stop)));
    return () => {
      disposed = true;
      stops.forEach((stop) => stop());
    };
  });
}

/**
 * Loads `fetch(source())` whenever the source changes. It stays outside Solid's async tracking, so the
 * change that triggered it renders at once. Reads `undefined` while loading and `null` if it failed.
 */
export function createFetch<S, T>(source: () => S, fetch: (source: S) => Promise<T>): Accessor<T | null | undefined> {
  const [value, setValue] = createSignal<T | null | undefined>();
  createEffect(source, (current) => {
    let live = true;
    setValue(undefined);
    fetch(current).then(
      (result) => live && setValue(() => result),
      () => live && setValue(null),
    );
    return () => {
      live = false;
    };
  });
  return value;
}

export function onEvent<T>(
  name: "routines_changed" | "workflow_progress" | "workflows_changed" | "themes_changed" | "covens_changed" | "vault_changed" | "approval_requested" | "approval_closed" | "agent_model" | "agents_changed" | "open_agent" | "dock_edge",
  handler: (payload: T) => void,
) {
  return listen<T>(name, (e) => handler(e.payload));
}

export interface DeltaEvent {
  agentId: string;
  messageId: number;
  chunk: string;
}

export interface StatusEvent {
  agentId: string;
  busy: boolean;
}

export async function subscribe(handlers: {
  message: (m: Message) => void;
  delta: (d: DeltaEvent) => void;
  status: (s: StatusEvent) => void;
}): Promise<UnlistenFn> {
  const stops = await Promise.all([
    listen<Message>("message", (e) => handlers.message(e.payload)),
    listen<DeltaEvent>("delta", (e) => handlers.delta(e.payload)),
    listen<StatusEvent>("agent_status", (e) => handlers.status(e.payload)),
  ]);
  return () => stops.forEach((stop) => stop());
}

export function errorText(error: unknown): string {
  return typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.";
}

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export function describeSchedule(schedule: Schedule): string {
  if (schedule.kind === "interval") {
    const { minutes } = schedule;
    if (minutes % 1440 === 0) return minutes === 1440 ? "Every day" : `Every ${minutes / 1440} days`;
    if (minutes % 60 === 0) return minutes === 60 ? "Every hour" : `Every ${minutes / 60} hours`;
    return `Every ${minutes} minutes`;
  }
  const days = [...schedule.days].sort();
  const when = `at ${formatClock(schedule.time)}`;
  if (days.length === 0 || days.length === 7) return `Every day ${when}`;
  if (days.join() === "1,2,3,4,5") return `Weekdays ${when}`;
  if (days.join() === "6,7") return `Weekends ${when}`;
  return `${days.map((d) => WEEKDAYS[d - 1]).join(", ")} ${when}`;
}

function formatClock(time: string): string {
  const [h, m] = time.split(":").map(Number);
  return new Date(2000, 0, 1, h, m).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function relativeTime(ms: number, now = Date.now()): string {
  const diff = ms - now;
  const abs = Math.abs(diff);
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["day", 86_400_000],
    ["hour", 3_600_000],
    ["minute", 60_000],
  ];
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  for (const [unit, size] of units) {
    // A timer set for an hour is a few seconds short of it by the time it shows; still call it an hour.
    if (abs >= size * 0.98) return format.format(Math.round(diff / size), unit);
  }
  return diff >= 0 ? "in a moment" : "just now";
}

export function shortPath(path: string): string {
  return path.replace(/^\/Users\/[^/]+/, "~");
}
