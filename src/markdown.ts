import { Marked } from "marked";
import { openUrl } from "@tauri-apps/plugin-opener";

const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

export function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

const SAFE_LINK = /^(https?:|mailto:)/i;

const marked = new Marked({
  gfm: true,
  breaks: true,
  async: false,
  renderer: {
    html({ text }) {
      return escapeHtml(text);
    },
    image({ text }) {
      return escapeHtml(text);
    },
    link({ href, tokens }) {
      const label = this.parser.parseInline(tokens);
      if (!SAFE_LINK.test(href)) return label;
      return `<a href="${escapeHtml(href)}">${label}</a>`;
    },
  },
});

export function renderMarkdown(text: string): string {
  return marked.parse(text) as string;
}

export function openLinks(event: MouseEvent) {
  const anchor = (event.target as HTMLElement).closest("a");
  if (!anchor) return;
  event.preventDefault();
  const href = anchor.getAttribute("href") ?? "";
  if (SAFE_LINK.test(href)) void openUrl(href);
}
