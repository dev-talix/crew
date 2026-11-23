import type { DraftChannel } from "./api";

/** Mail apps and macOS can drop links much longer than this, so a longer body goes by clipboard. */
export const MAILTO_LIMIT = 2000;

/** Recipients typed into one box, split on commas, semicolons, or new lines. */
export function parseRecipients(text: string): string[] {
  return text
    .split(/[,;\n]/)
    .map((t) => t.trim())
    .filter(Boolean);
}

/**
 * A mailto: link for an email draft. When the body would make it too long, the link leaves the body
 * out and `bodyIncluded` is false, so the caller can copy it instead.
 */
export function mailtoUrl(to: string[], subject: string, body: string): { url: string; bodyIncluded: boolean } {
  const address = (a: string) => encodeURIComponent(a).replace(/%40/g, "@");
  // RFC 6068: line breaks in a mailto: body are CRLF.
  const encode = (text: string) => encodeURIComponent(text.replace(/\r?\n/g, "\r\n"));
  const base = `mailto:${to.map(address).join(",")}`;
  const query = (parts: string[]) => (parts.length ? `${base}?${parts.join("&")}` : base);
  const head = subject.trim() ? [`subject=${encode(subject.trim())}`] : [];
  const full = query(body ? [...head, `body=${encode(body)}`] : head);
  if (full.length <= MAILTO_LIMIT) return { url: full, bodyIncluded: true };
  return { url: query(head), bodyIncluded: false };
}

/** "Email to a@example.com", "Slack to #eng", or "Post for LinkedIn". */
export function draftTitle(channel: DraftChannel, to: string[]): string {
  const list = to.join(", ");
  if (channel === "email") return `Email to ${list}`;
  if (channel === "slack") return `Slack to ${list}`;
  return list ? `Post for ${list}` : "Post";
}
