import type { InboxItem, InboxOption } from "./api";

export interface InboxGroup {
  agentId: string;
  items: InboxItem[];
}

export const isApproval = (item: InboxItem) => item.kind !== "question";

/** The item's options with the recommended one first, the rest in their original order. */
export function optionsInOrder(item: InboxItem): InboxOption[] {
  const recommended = item.options.filter((o) => o.value === item.recommended);
  return [...recommended, ...item.options.filter((o) => o.value !== item.recommended)];
}

/**
 * Items grouped by familiar. Approvals come first everywhere, since they time out in two minutes;
 * then questions, oldest first. A familiar with an approval waiting comes before one without.
 */
export function groupByFamiliar(items: InboxItem[]): InboxGroup[] {
  const sorted = [...items].sort((a, b) => Number(isApproval(b)) - Number(isApproval(a)) || (a.createdAt ?? 0) - (b.createdAt ?? 0));
  const groups = new Map<string, InboxItem[]>();
  for (const item of sorted) groups.set(item.agentId, [...(groups.get(item.agentId) ?? []), item]);
  return [...groups].map(([agentId, items]) => ({ agentId, items }));
}
