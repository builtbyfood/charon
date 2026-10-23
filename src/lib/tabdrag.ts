// Tab bar drag helpers (App.svelte): pure so they are node-tested.

/** Where a tab dropped at client x lands among the other tabs (their rects, in order). */
export function dropIndex(rects: { left: number; right: number }[], x: number): number {
  for (let i = 0; i < rects.length; i++) {
    if (x < (rects[i].left + rects[i].right) / 2) return i;
  }
  return rects.length;
}

/** `list` with the item `id` moved to index `to` of the list without it. Same array back when nothing moves. */
export function reorder<T extends { id: string }>(list: T[], id: string, to: number): T[] {
  const from = list.findIndex((t) => t.id === id);
  if (from < 0) return list;
  const rest = list.filter((t) => t.id !== id);
  const at = Math.max(0, Math.min(rest.length, to));
  if (at === from) return list;
  return [...rest.slice(0, at), list[from], ...rest.slice(at)];
}
