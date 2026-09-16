// Frontend mirror of connections.rs inheritance, used to show what a folder
// contributes to the nodes under it. The backend's `effective()` is authoritative.
import type { Node, Settings } from "./api";

/** `over` wins wherever it has a value (same rule as `Settings::merged_with`). */
export function mergeSettings(base: Settings, over: Settings): Settings {
  const out: Settings = { ...base };
  for (const [k, v] of Object.entries(over)) {
    if (v !== undefined && v !== null && v !== "") (out as Record<string, unknown>)[k] = v;
  }
  return out;
}

/** Settings inherited *by* a child of `parentId`: root folder → … → parent. */
export function inheritedSettings(nodes: Node[], parentId: string | null): Settings {
  const chain: Node[] = [];
  let cur = parentId ? nodes.find((n) => n.id === parentId) : undefined;
  while (cur) {
    chain.push(cur);
    const pid = cur.parent_id;
    cur = pid ? nodes.find((n) => n.id === pid) : undefined;
  }
  return chain.reverse().reduce((acc, n) => mergeSettings(acc, n.settings), {} as Settings);
}

/** Effective settings of `node` itself. */
export const effectiveSettings = (nodes: Node[], node: Node): Settings =>
  mergeSettings(inheritedSettings(nodes, node.parent_id), node.settings);

/** Preset colours for the swatch row: distinct on a dark background. */
export const PALETTE = ["#6aa2ff", "#5fd38a", "#ffb454", "#ff6b6b", "#c792ea", "#4dd0e1", "#f78c6c", "#9aa5b1"];
