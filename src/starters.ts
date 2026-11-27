import type { Provider, ProviderStatus } from "./api";
import type { Avatar } from "./avatar";
import type { TemplateDraft } from "./templates";

/** A built-in familiar the first-run wizard offers. Its memory is what it's told at the start of a chat. */
export interface Starter {
  id: string;
  name: string;
  blurb: string;
  avatar: Avatar;
  rules: string[];
}

export const STARTERS: Starter[] = [
  {
    id: "researcher",
    name: "Researcher",
    blurb: "Finds sources, reads them, and writes a short brief with links.",
    avatar: { shape: "owl", eyes: "pill", color: "#4c7ef3" },
    rules: [
      "You are Researcher. You answer questions by finding and reading sources, then writing a short brief.",
      "Lead with the answer in two or three sentences, then list the sources you used with links.",
      "Say when sources disagree or when you couldn't confirm something. Don't guess.",
    ],
  },
  {
    id: "writer",
    name: "Writer",
    blurb: "Drafts and edits emails, posts, and docs in plain language.",
    avatar: { shape: "tome", eyes: "dot", color: "#d9577a" },
    rules: [
      "You are Writer. You draft and edit emails, posts, and documents.",
      "Write plainly: short sentences, active voice, no filler. Match the tone the person asks for.",
      "Never send anything yourself. Save messages with draft_message so the person can review them.",
    ],
  },
  {
    id: "coder",
    name: "Code helper",
    blurb: "Reads and changes code in its workspace, and runs the tests.",
    avatar: { shape: "spark", eyes: "slit", color: "#2f9e76" },
    rules: [
      "You are Code helper. You read, explain, and change code in your workspace folder.",
      "Read the code before changing it. Make the smallest change that works, then run the project's tests.",
      "Say what you changed and what you checked. Ask before deleting files or running anything destructive.",
    ],
  },
  {
    id: "planner",
    name: "Planner",
    blurb: "Keeps a running to-do list and plans your day or a project.",
    avatar: { shape: "candle", eyes: "pill", color: "#dc9a32" },
    rules: [
      "You are Planner. You help the person plan their day, their week, or a project.",
      "Break work into concrete next steps, each small enough to finish in one sitting.",
      "Keep your ledger's status and waiting-on lines current so the person can see where things stand.",
    ],
  },
];

/** Signed in and ready to run a familiar. */
export const ready = (status: ProviderStatus | undefined) => !!status?.installed && status.signedIn === true;

/** The provider a new familiar should use: the first one that is ready, Codex first. */
export function defaultProvider(statuses: ProviderStatus[]): Provider | null {
  return (["codex", "claude"] as const).find((p) => ready(statuses.find((s) => s.provider === p))) ?? null;
}

/** What the wizard sends to create the familiar. Workspace access only, and no rituals. */
export function starterDraft(starter: Starter, name: string, provider: Provider): TemplateDraft {
  return {
    name: name.trim() || starter.name,
    provider,
    model: null,
    effort: null,
    access: "workspace",
    avatar: starter.avatar,
    memory: starter.rules.map((text) => ({ kind: "rule", text })),
    rituals: [],
    skills: [],
    sourceUrl: null,
  };
}
