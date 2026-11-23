import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { createEffect, createSignal, onSettled, type Accessor } from "solid-js";
import type { Avatar } from "./avatar";
import type { ThemeFile } from "./theme";
import type { Memory, MemoryDraft, MemoryKind, MemoryScope } from "./memory";
import type * as Budgets from "./budgets";
import type * as Transfer from "./transfer";
import type { Ledger } from "./ledgers";

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
  /** "side" links to a side conversation with another familiar; its source holds that chat's id. */
  role: "user" | "assistant" | "tool" | "error" | "side";
  text: string;
  status: "done" | "streaming" | "queued" | "cancelled" | "stopped" | "interrupted";
  source: string | null;
  createdAt: number;
  updatedAt: number | null;
  /** Files and images sent with a user message. */
  attachments?: Attachment[];
}

/** Two familiars talking behind the scenes: a hidden chat of the familiar that was asked. */
export interface SideChat {
  id: string;
  askerId: string;
  targetId: string;
  title: string;
  sessionId: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface SideThread {
  chat: SideChat;
  messages: Message[];
}

export interface SideMessageEvent {
  chatId: string;
  message: Message;
}

export interface SideDeltaEvent {
  chatId: string;
  messageId: number;
  chunk: string;
}

/** One of a familiar's conversations, with its own CLI session. */
export interface ChatInfo {
  id: string;
  agentId: string;
  /** Empty until the first message. */
  title: string;
  sessionId: string | null;
  createdAt: number;
  updatedAt: number;
  current: boolean;
}

export interface SearchHit {
  messageId: number;
  agentId: string;
  chatId: string;
  chatTitle: string;
  role: "user" | "assistant";
  /** The text around the match, each matched term between \u0002 and \u0003. */
  snippet: string;
  createdAt: number;
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

export type Schedule =
  | { kind: "interval"; minutes: number }
  | { kind: "daily"; time: string; days: number[] }
  /** Runs when a pull request opens in `repo`, "owner/name". */
  | { kind: "githubPr"; repo: string }
  /** Runs when a file, or what's directly in a folder, changes. */
  | { kind: "file"; path: string }
  /** Runs when something POSTs to its local webhook. Grimoire makes the secret; send "" for a new one. */
  | { kind: "webhook"; secret: string }
  /** Runs when new mail reaches the Mail app's inbox. */
  | { kind: "email" };

/** How an event ritual's watcher is doing. */
export interface TriggerState {
  routineId: string;
  checkedAt: number | null;
  firedAt: number | null;
  error: string | null;
}

export interface TriggerStatus {
  /** Add a webhook ritual's secret to get its address. */
  webhookBase: string;
  /** Why webhooks are off, if they are. */
  webhookError: string | null;
  states: TriggerState[];
}

export interface Routine {
  id: string;
  /** The familiar this ritual messages, or null when it casts a spell. */
  agentId: string | null;
  /** The spell it casts, with `prompt` as the optional starting text. */
  workflowId: string | null;
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
  agentId: string | null;
  workflowId: string | null;
  name: string;
  prompt: string;
  schedule: Schedule;
  enabled: boolean;
}

export interface WorkflowStep {
  agentId: string;
  prompt: string;
  /** Runs alongside the previous step instead of after it. */
  parallel?: boolean;
  check?: StepCheck | null;
}

/** After a step, goes to step `goTo` (an index) when the reply contains, or lacks, `phrase`. */
export interface StepCheck {
  when: "contains" | "lacks";
  phrase: string;
  goTo: number;
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
  runId: string;
  /** The first step running now, counting from 1. */
  step: number;
  total: number;
  agentId: string | null;
  /** Every step running now, counting from 1. */
  steps: number[];
  agents: string[];
  state: "running" | "done" | "failed" | "stopped";
  error: string | null;
}

export type RunState = "running" | "done" | "failed" | "stopped";

/** One cast of a spell. */
export interface WorkflowRun {
  id: string;
  workflowId: string;
  input: string;
  /** The ritual that cast it, by name. */
  ritual: string | null;
  state: RunState;
  error: string | null;
  startedAt: number;
  endedAt: number | null;
}

export interface WorkflowRunStep {
  id: number;
  /** Index of the step in the spell. */
  step: number;
  agentId: string;
  /** As sent, with {{input}} and {{previous}} filled in. */
  prompt: string;
  reply: string | null;
  state: RunState;
  error: string | null;
  /** Index of the step this one's check sent the spell to. */
  wentTo: number | null;
  startedAt: number;
  endedAt: number | null;
}

export interface WorkflowRunDetail extends WorkflowRun {
  steps: WorkflowRunStep[];
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

export interface NotificationPrefs {
  replies: boolean;
  approvals: boolean;
  /** Rituals and spells. */
  runs: boolean;
  backgroundOnly: boolean;
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
  kind: "vault" | "command" | "file" | "familiar";
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

/** Who a familiar may ask with ask_familiar, and whether the person approves each question. */
export interface Delegation {
  scope: "anyone" | "coven" | "nobody";
  approve: boolean;
}

/** A short-lived helper a familiar started for a long job. Its transcript is a side chat. */
export interface Helper {
  id: string;
  agentId: string;
  chatId: string;
  label: string;
  task: string;
  status: "running" | "done" | "failed" | "stopped" | "timeout" | "interrupted";
  result: string | null;
  startedAt: number;
  finishedAt: number | null;
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
  newChat: (agentId: string) => invoke<ChatInfo>("new_chat", { agentId }),
  listChats: (agentId: string) => invoke<ChatInfo[]>("list_chats", { agentId }),
  openChat: (agentId: string, chatId: string) => invoke<ChatInfo>("open_chat", { agentId, chatId }),
  renameChat: (chatId: string, title: string) => invoke<ChatInfo>("rename_chat", { chatId, title }),
  deleteChat: (chatId: string) => invoke<void>("delete_chat", { chatId }),
  searchMessages: (query: string) => invoke<SearchHit[]>("search_messages", { query, limit: 30 }),
  sideThread: (chatId: string) => invoke<SideThread>("side_thread", { chatId }),
  lastMessages: () => invoke<Message[]>("last_messages"),
  listModels: (provider: Provider) => invoke<ModelOption[]>("list_models", { provider }),
  dockEdge: () => invoke<"left" | "right" | null>("dock_edge"),
  dockResize: (width: number, height: number, focus: boolean) => invoke<void>("dock_resize", { width, height, focus }),
  dockHide: () => invoke<void>("dock_hide"),
  /** The familiars left out of the dock. */
  dockHidden: () => invoke<string[]>("dock_hidden"),
  setDockHidden: (ids: string[]) => invoke<string[]>("set_dock_hidden", { ids }),
  showMain: (agentId?: string) => invoke<void>("show_main", { agentId: agentId ?? null }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  providerStatus: () => invoke<ProviderStatus[]>("provider_status"),
  listRoutines: () => invoke<Routine[]>("list_routines"),
  saveRoutine: (input: RoutineInput) => invoke<Routine>("save_routine", { input }),
  deleteRoutine: (id: string) => invoke<void>("delete_routine", { id }),
  runRoutine: (id: string) => invoke<void>("run_routine", { id }),
  triggerStatus: () => invoke<TriggerStatus>("trigger_status"),
  listWorkflows: () => invoke<Workflow[]>("list_workflows"),
  runningWorkflows: () => invoke<string[]>("running_workflows"),
  saveWorkflow: (input: WorkflowInput) => invoke<Workflow>("save_workflow", { input }),
  deleteWorkflow: (id: string) => invoke<void>("delete_workflow", { id }),
  /** Starts a cast and returns its run id. */
  runWorkflow: (id: string, input: string) => invoke<string>("run_workflow", { id, input }),
  stopWorkflow: (id: string) => invoke<void>("stop_workflow", { id }),
  recentWorkflowRuns: () => invoke<WorkflowRun[]>("recent_workflow_runs"),
  getWorkflowRun: (id: string) => invoke<WorkflowRunDetail>("get_workflow_run", { id }),
  listThemes: () => invoke<ThemeList>("list_themes"),
  saveTheme: (id: string | null, theme: ThemeFile) => invoke<ThemeEntry>("save_theme", { id, theme }),
  deleteTheme: (id: string) => invoke<void>("delete_theme", { id }),
  setTheme: (id: string) => invoke<void>("set_theme", { id }),
  openThemesFolder: () => invoke<void>("open_themes_folder"),
  setAppIcon: (icon: "paper" | "night") => invoke<void>("set_app_icon", { icon }),
  listCovens: () => invoke<Coven[]>("list_covens"),
  saveCoven: (id: string | null, name: string, members: string[]) => invoke<Coven>("save_coven", { id, name, members }),
  deleteCoven: (id: string) => invoke<void>("delete_coven", { id }),
  getMemory: (scope: MemoryScope, id: string) => invoke<Memory>("get_memory", { scope, id }),
  saveMemory: (scope: MemoryScope, id: string, entries: MemoryDraft[], removed: string[]) => invoke<Memory>("save_memory", { scope, id, entries, removed }),
  addMemory: (scope: MemoryScope, id: string, kind: MemoryKind, project: string | null, text: string) =>
    invoke<Memory>("add_memory", { scope, id, kind, project, text }),
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
  getDelegation: () => invoke<Delegation>("get_delegation"),
  saveDelegation: (settings: Delegation) => invoke<Delegation>("save_delegation", { settings }),
  notificationSettings: () => invoke<NotificationPrefs>("notification_settings"),
  saveNotificationSettings: (prefs: NotificationPrefs) => invoke<NotificationPrefs>("save_notification_settings", { prefs }),
  menuBarVisible: () => invoke<boolean>("menu_bar_visible"),
  setMenuBarVisible: (visible: boolean) => invoke<boolean>("set_menu_bar_visible", { visible }),
  /** Tells the menu bar which familiar's chat the main window shows. */
  setOpenAgent: (agentId: string | null) => invoke<void>("set_open_agent", { agentId }),
  listBudgets: () => invoke<Budgets.BudgetStatus[]>("list_budgets"),
  setBudget: (agentId: string, budget: Budgets.Budget | null) => invoke<void>("set_budget", { agentId, budget }),
  getPlanGuard: () => invoke<Budgets.PlanGuard>("get_plan_guard"),
  savePlanGuard: (guard: Budgets.PlanGuard) => invoke<Budgets.PlanGuard>("save_plan_guard_setting", { guard }),
  ritualPause: () => invoke<string | null>("ritual_pause"),
  ritualSkips: () => invoke<Budgets.RitualSkip[]>("ritual_skips"),
  /** Asks where to save, then writes the file. Null when cancelled. */
  exportItem: (kind: Transfer.ExportKind, id: string) => invoke<string | null>("export_item", { kind, id }),
  exportBackup: (includeHistory: boolean) => invoke<string | null>("export_backup_file", { includeHistory }),
  /** Asks for a file and previews it. Null when cancelled. */
  pickImport: () => invoke<Transfer.Picked | null>("pick_import"),
  applyImport: (keepFullAccess: boolean) => invoke<Transfer.Imported>("apply_import", { keepFullAccess }),
  /** Open questions from familiars and pending approvals, in one list. */
  inbox: () => invoke<InboxItem[]>("inbox"),
  /** An option's value, or the person's own words. */
  answerQuestion: (id: string, answer: string) => invoke<void>("answer_question", { id, answer }),
  dismissQuestion: (id: string) => invoke<void>("dismiss_question", { id }),
  getLedger: (agentId: string) => invoke<Ledger>("get_ledger", { agentId }),
  /** Fields left out stay as they are; an empty string clears one. */
  saveLedger: (agentId: string, patch: Partial<Pick<Ledger, "goal" | "status" | "waitingOn" | "nextStep">>) =>
    invoke<Ledger>("save_ledger", { agentId, patch }),
  listHelpers: (agentId: string) => invoke<Helper[]>("list_helpers", { agentId }),
  stopHelper: (id: string) => invoke<void>("stop_helper", { id }),
  listDrafts: () => invoke<Draft[]>("list_drafts"),
  /** Marks a draft sent, with the person's edits, or discarded. The familiar hears which. */
  closeDraft: (id: string, sent: boolean, edits: DraftEdits | null) => invoke<Draft>("close_draft", { id, sent, edits }),
  openDraftLink: (id: string, url: string) => invoke<void>("open_draft_link", { id, url }),
};

export interface InboxOption {
  /** The option text for a question, or once/always/deny for an approval. */
  value: string;
  label: string;
}

/** A familiar's question from ask_person, or a pending approval. */
export interface InboxItem {
  id: string;
  kind: "question" | ApprovalRequest["kind"];
  agentId: string;
  title: string;
  detail: string;
  options: InboxOption[];
  /** The value of the recommended option. */
  recommended: string;
  /** Null for approvals. */
  createdAt: number | null;
}

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
  name: "routines_changed" | "workflow_progress" | "workflows_changed" | "themes_changed" | "covens_changed" | "memory_changed" | "vault_changed" | "approval_requested" | "approval_closed" | "agent_model" | "agents_changed" | "open_agent" | "dock_edge" | "dock_hidden" | "chats_changed" | "budgets_changed" | "side_message" | "side_delta" | "inbox_changed" | "ledger_changed" | "helpers_changed" | "drafts_changed",
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
  if (schedule.kind === "githubPr") return `When a PR opens in ${schedule.repo}`;
  if (schedule.kind === "file") return `When ${shortPath(schedule.path)} changes`;
  if (schedule.kind === "webhook") return "When its webhook is called";
  if (schedule.kind === "email") return "When new email arrives";
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

export type DraftChannel = "email" | "slack" | "post";

export interface DraftEdits {
  to: string[];
  subject: string | null;
  body: string;
}

export interface Draft extends DraftEdits {
  id: string;
  agentId: string;
  channel: DraftChannel;
  note: string | null;
  state: "draft" | "sent" | "discarded";
  createdAt: number;
  closedAt: number | null;
}
