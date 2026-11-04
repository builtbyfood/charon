// File pane layout: the pane's width beside the terminal, and the list's columns
// (widths, hidden set). Both persist in the profile's [files] section.

/** Pane width when nothing is saved, and what a double-click on the divider restores. */
export const PANE_DEFAULT = 420;
export const PANE_MIN = 240;
/** The terminal keeps at least this much beside the pane (~40 columns at 13px). */
export const TERM_MIN = 320;
/** The divider between them. */
export const DIVIDER = 4;

/** Pane width that leaves both sides their minimum in a row `total` px wide (0 = not
 *  laid out yet). Too narrow for both: the pane keeps its minimum. */
export function clampPane(w: number, total: number): number {
  const want = Number.isFinite(w) && w > 0 ? w : PANE_DEFAULT;
  const max = total > 0 ? total - TERM_MIN - DIVIDER : Infinity;
  return Math.round(Math.max(PANE_MIN, Math.min(want, max)));
}

export type ColKey = "name" | "size" | "mtime" | "mode";
export interface ColDef { key: ColKey; label: string; min: number; def: number }
/** Name's `def` is its minimum while it fills the free space (no width saved). */
export const COLS: ColDef[] = [
  { key: "name", label: "Name", min: 80, def: 80 },
  { key: "size", label: "Size", min: 40, def: 64 },
  { key: "mtime", label: "Modified", min: 60, def: 118 },
  { key: "mode", label: "Perm", min: 50, def: 84 },
];
const COL_MAX = 1200;
/** Grid gap and the row's left + right padding (SftpPane's .head / .row). */
export const COL_GAP = 6;
export const ROW_PAD = 16;

export interface ColLayout {
  /** px per column; Name absent (or 0) = Name fills whatever the others leave. */
  widths: Partial<Record<ColKey, number>>;
  hidden: ColKey[];
}
export const DEFAULT_COLS: ColLayout = { widths: {}, hidden: [] };

const isKey = (k: string): k is ColKey => COLS.some((c) => c.key === k);
const def = (k: ColKey) => COLS.find((c) => c.key === k)!;

/** Accept whatever the profile holds: unknown keys and out-of-range widths dropped, Name never hidden. */
export function colsFromProfile(widths: unknown, hidden: unknown): ColLayout {
  const w: Partial<Record<ColKey, number>> = {};
  if (widths && typeof widths === "object") {
    for (const [k, v] of Object.entries(widths as Record<string, unknown>)) {
      if (isKey(k) && typeof v === "number" && Number.isFinite(v) && v > 0) w[k] = clampCol(k, v);
    }
  }
  const h = Array.isArray(hidden) ? [...new Set(hidden.filter((k): k is ColKey => typeof k === "string" && isKey(k) && k !== "name"))] : [];
  return { widths: w, hidden: h };
}

export function clampCol(key: ColKey, w: number): number {
  return Math.round(Math.max(def(key).min, Math.min(COL_MAX, w)));
}

/** The columns on screen: not hidden, and Perm only where the listing has mode bits. */
export function visibleCols(l: ColLayout, hasModes: boolean): ColDef[] {
  return COLS.filter((c) => c.key === "name" || (!l.hidden.includes(c.key) && (c.key !== "mode" || hasModes)));
}

const widthOf = (l: ColLayout, c: ColDef) => l.widths[c.key] || c.def;

/** grid-template-columns for the header and every row. */
export function gridTemplate(l: ColLayout, cols: ColDef[]): string {
  return cols.map((c) => c.key === "name" && !l.widths.name ? `minmax(${c.def}px, 1fr)` : `${widthOf(l, c)}px`).join(" ");
}

/** A row's minimum width: past it the list scrolls sideways instead of squeezing columns. */
export function rowMinWidth(l: ColLayout, cols: ColDef[]): number {
  return cols.reduce((s, c) => s + widthOf(l, c), 0) + COL_GAP * Math.max(0, cols.length - 1) + ROW_PAD;
}

/** Show or hide a column; Name cannot be hidden. */
export function toggleCol(l: ColLayout, key: ColKey): ColLayout {
  if (key === "name") return l;
  const hidden = l.hidden.includes(key) ? l.hidden.filter((k) => k !== key) : [...l.hidden, key];
  return { ...l, hidden };
}

export function setColWidth(l: ColLayout, key: ColKey, w: number): ColLayout {
  return { ...l, widths: { ...l.widths, [key]: clampCol(key, w) } };
}

/** Auto-fit: the widest measured cell (header included) plus a little air. */
export function fitCol(key: ColKey, measured: number[]): number {
  return clampCol(key, Math.ceil(Math.max(0, ...measured)) + 8);
}
