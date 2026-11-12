/**
 * Split view (several terminals side by side): grid sizing and the per-tab
 * Files-pane state that Split collapses and gives back. Pure, node-tested.
 */

export interface GridLayout {
  cols: number;
  /** Rows in the whole grid. */
  rows: number;
  /** Rows on screen at once (= rows unless it scrolls). */
  visibleRows: number;
  /** Height of each pane in px. */
  paneH: number;
  /** More rows than fit: the grid scrolls vertically, snapping to rows. */
  scroll: boolean;
}

export interface GridOpts {
  /** Smallest useful terminal: this many text rows / columns per pane. */
  minRows: number;
  minCols: number;
  /** Per pane, px that aren't text cells (status bar, padding) — vertically / horizontally. */
  chromeH: number;
  chromeW: number;
  /** Gap between panes, px. */
  gap: number;
}
export const GRID_DEFAULTS: GridOpts = { minRows: 24, minCols: 60, chromeH: 30, chromeW: 20, gap: 3 };

/**
 * Columns and rows for `n` panes in a `width`×`height` area with `cell`-sized
 * characters. Every arrangement whose panes all keep at least `minRows`×`minCols`
 * is a candidate; the one whose panes are roomiest (the smaller of the two
 * headroom ratios) wins, fewer empty cells breaking ties. If none fits, as many
 * columns as the width allows and minimum-height rows that exactly fill the
 * height, scrolling.
 */
export function gridLayout(n: number, width: number, height: number, cell: { w: number; h: number }, o: GridOpts = GRID_DEFAULTS): GridLayout {
  const k = Math.max(1, Math.floor(n));
  const minH = o.minRows * cell.h + o.chromeH;
  const minW = o.minCols * cell.w + o.chromeW;
  const maxCols = Math.max(1, Math.min(k, Math.floor((width + o.gap) / (minW + o.gap))));
  let best: { cols: number; rows: number; paneH: number; score: number; empty: number } | null = null;
  for (let cols = 1; cols <= maxCols; cols++) {
    const rows = Math.ceil(k / cols);
    const paneH = (height - o.gap * (rows - 1)) / rows;
    const paneW = (width - o.gap * (cols - 1)) / cols;
    if (paneH < minH) continue;
    const score = Math.min(paneW / minW, paneH / minH);
    const empty = rows * cols - k;
    if (!best || score > best.score + 1e-9 || (Math.abs(score - best.score) <= 1e-9 && empty < best.empty)) {
      best = { cols, rows, paneH, score, empty };
    }
  }
  if (best) return { cols: best.cols, rows: best.rows, visibleRows: best.rows, paneH: Math.floor(best.paneH), scroll: false };
  const cols = maxCols;
  const rows = Math.ceil(k / cols);
  const visibleRows = Math.max(1, Math.min(rows, Math.floor((height + o.gap) / (minH + o.gap))));
  // Fill the height exactly so a snapped scroll shows whole rows; never below the minimum.
  const paneH = Math.max(minH, (height - o.gap * (visibleRows - 1)) / visibleRows);
  return { cols, rows, visibleRows, paneH: Math.floor(paneH), scroll: rows > visibleRows };
}

/**
 * Files panes while in Split. `saved` holds each split tab's Files state from
 * before it joined the grid. A tab joining is recorded and collapsed; a tab
 * leaving (unticked, closed, Split turned off: `ids` = []) gets its own state
 * back. Returns the new record and the Files state to apply per tab.
 */
export function syncSplitFiles(saved: Readonly<Record<string, boolean>>, ids: readonly string[], isOpen: (id: string) => boolean):
  { saved: Record<string, boolean>; set: Record<string, boolean> } {
  const next: Record<string, boolean> = {};
  const set: Record<string, boolean> = {};
  for (const id of ids) {
    if (id in saved) next[id] = saved[id];
    else { next[id] = isOpen(id); if (next[id]) set[id] = false; }
  }
  for (const [id, was] of Object.entries(saved)) {
    if (!ids.includes(id) && isOpen(id) !== was) set[id] = was;
  }
  return { saved: next, set };
}

/** The top bar's Files toggle in Split: any pane open → close all, else open all. */
export function filesAllTarget(states: readonly boolean[]): boolean {
  return !states.some(Boolean);
}

/** Sessions a broadcast reaches: the ticked tabs (any window) plus the source. */
export function broadcastCount(selected: Iterable<string>, source: string | null): number {
  const s = new Set(selected);
  if (source) s.add(source);
  return s.size;
}
