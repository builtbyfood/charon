// Tabs reopened at the next start. Each window reports its terminal tabs while it
// runs (backend `opentabs.rs` keeps them in data/open-tabs.json); at start the
// main window reopens every window's tabs. Pure so it can be node-tested.
import type { Node } from "./api";
import type { SessionFont } from "./prefs";
import { colorOf, fontOf } from "./tree.ts";

/** What a tab needs to be opened again (OpenTarget's shape, kept local so this
 *  module does not import a .svelte file). */
export interface SavedTarget {
  kind: "ssh" | "local" | "serial";
  nodeId?: string; host?: string; user?: string; port?: number;
  admin?: boolean;
}
export interface SavedTab { title: string; color?: string; font?: SessionFont; open: SavedTarget }
export interface SavedWindow { tabs: SavedTab[]; active: number }

/** A window's tabs worth reopening: terminals only (the editor and its files are
 *  not kept), and not elevated local shells, which would prompt for UAC / sudo at
 *  start. `activeId` = the window's visible tab. */
export function toSaved(tabs: { id: string; title: string; color?: string; font?: SessionFont; open?: SavedTarget }[],
  activeId: string | null): SavedWindow {
  const kept = tabs.filter((t) => t.open && !t.open.admin);
  const saved = kept.map((t) => {
    const { kind, nodeId, host, user, port } = t.open!;
    const open: SavedTarget = nodeId ? { kind, nodeId } : { kind, host, user, port };
    return { title: t.title, color: t.color, font: t.font, open };
  });
  return { tabs: saved, active: Math.max(0, kept.findIndex((t) => t.id === activeId)) };
}

/** Everything the last run had open, as one list for the main window: its own
 *  tabs first, then each torn-off window's. Saved connections are looked up
 *  again so a rename, recolour or font change since then applies, and a deleted
 *  one is dropped. `active` indexes the result (the main window's visible tab). */
export function restorePlan(saved: Record<string, unknown>, nodes: Node[]): { tabs: SavedTab[]; active: number } {
  const labels = Object.keys(saved).sort((a, b) => (a === "main" ? -1 : b === "main" ? 1 : a.localeCompare(b)));
  const out: SavedTab[] = [];
  let active = 0;
  for (const label of labels) {
    const w = saved[label] as Partial<SavedWindow> | null;
    if (!w || !Array.isArray(w.tabs)) continue;
    w.tabs.forEach((t, i) => {
      const tab = revive(t, nodes);
      if (!tab) return;
      if (label === "main" && i === w.active) active = out.length;
      out.push(tab);
    });
  }
  return { tabs: out, active };
}

function revive(t: SavedTab | null | undefined, nodes: Node[]): SavedTab | null {
  const o = t?.open;
  if (!o || !["ssh", "local", "serial"].includes(o.kind) || o.admin) return null;
  if (o.nodeId) {
    const n = nodes.find((x) => x.id === o.nodeId);
    if (!n || n.kind === "folder") return null;
    return { title: n.name, color: colorOf(nodes, n), font: fontOf(nodes, n), open: { kind: o.kind, nodeId: n.id } };
  }
  if (o.kind === "ssh" && !o.host) return null;
  return { title: typeof t.title === "string" && t.title ? t.title : o.kind, color: t.color, font: t.font, open: o };
}
