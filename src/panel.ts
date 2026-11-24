export const PANEL_MIN = 320;
export const PANEL_DEFAULT = 440;
/** The chat beside a side panel keeps at least this much room. */
export const CHAT_MIN = 360;
/** How much of the chat a panel floating over it leaves showing. */
export const OVERLAY_GAP = 48;

/** The widest a side panel may be: what leaves `reserve` for the page beside or under it, but never under its minimum. */
export const panelMax = (available: number, reserve = CHAT_MIN) => Math.max(PANEL_MIN, available - reserve);

/** A side panel's width, kept between its minimum and `panelMax`. */
export function clampPanelWidth(width: number, available: number, reserve = CHAT_MIN): number {
  return Math.round(Math.min(panelMax(available, reserve), Math.max(PANEL_MIN, width)));
}

/** A stored width, or the default when nothing usable is stored. */
export function parsePanelWidth(stored: string | null): number {
  const width = Number(stored);
  return stored && Number.isFinite(width) && width > 0 ? width : PANEL_DEFAULT;
}

/** A form field the user hasn't touched takes a change made elsewhere; an edited one keeps the edit. */
export const followUntouched = <T>(current: T, before: T, after: T): T => (current === before ? after : current);
