import { test } from "node:test";
import assert from "node:assert/strict";
import { GRID_DEFAULTS, broadcastCount, filesAllTarget, gridLayout, syncSplitFiles } from "./splitview.ts";

const cell = { w: 8, h: 17 };                        // 13px mono-ish
const minH = 24 * cell.h + GRID_DEFAULTS.chromeH;    // 438
const minW = 60 * cell.w + GRID_DEFAULTS.chromeW;    // 500
const shape = (l: { cols: number; rows: number }) => `${l.cols}x${l.rows}`;

test("grid: picks the roomiest arrangement that keeps every pane at the minimum", () => {
  // 1920×1000: two side by side, four 2×2, six 3×2, all on screen.
  assert.equal(shape(gridLayout(1, 1920, 1000, cell)), "1x1");
  assert.equal(shape(gridLayout(2, 1920, 1000, cell)), "2x1");
  assert.equal(shape(gridLayout(3, 1920, 1000, cell)), "3x1");
  assert.equal(shape(gridLayout(4, 1920, 1000, cell)), "2x2");
  assert.equal(shape(gridLayout(6, 1920, 1000, cell)), "3x2");
  for (const n of [1, 2, 3, 4, 6]) {
    const l = gridLayout(n, 1920, 1000, cell);
    assert.equal(l.scroll, false, `n=${n}`);
    assert.ok(l.paneH >= minH, `n=${n} paneH=${l.paneH}`);
    assert.ok(l.paneH * l.rows + GRID_DEFAULTS.gap * (l.rows - 1) <= 1000, `n=${n} fits the height`);
  }
  // Narrow and tall: stack instead of squeezing below 60 columns.
  assert.equal(shape(gridLayout(2, 700, 1000, cell)), "1x2");
  // Roomier panes beat a full grid: 3 panes on a tall screen take 2×2 with one cell empty.
  assert.equal(shape(gridLayout(3, 1600, 2000, cell)), "2x2");
});

test("grid: too many panes scroll, minimum-height rows filling the height exactly", () => {
  const l = gridLayout(9, 1920, 1000, cell);
  assert.equal(shape(l), "3x3");
  assert.equal(l.scroll, true);
  assert.equal(l.visibleRows, 2);
  assert.ok(l.paneH >= minH);
  assert.equal(l.paneH, Math.floor((1000 - GRID_DEFAULTS.gap) / 2), "two whole rows fill the view");
  // Shorter than one minimum pane: one row at a time, still the minimum height.
  const tiny = gridLayout(4, 1920, 300, cell);
  assert.equal(tiny.visibleRows, 1);
  assert.equal(tiny.paneH, minH);
  assert.equal(tiny.scroll, true);
  // Narrower than one minimum pane: one column.
  assert.equal(gridLayout(3, 300, 2000, cell).cols, 1);
  assert.ok(minW > 300);
});

test("Files state round-trips across Split enter / exit", () => {
  const open: Record<string, boolean> = { a: true, b: false, c: true, d: true };
  const isOpen = (id: string) => !!open[id];
  const apply = (set: Record<string, boolean>) => { for (const [id, v] of Object.entries(set)) open[id] = v; };

  // Enter with a, b, c: every Files pane collapses, the old state is kept.
  let r = syncSplitFiles({}, ["a", "b", "c"], isOpen);
  assert.deepEqual(r.set, { a: false, c: false });
  apply(r.set);
  assert.deepEqual(r.saved, { a: true, b: false, c: true });
  // In Split the user opens b and closes nothing; d joins (collapsed, remembered).
  open.b = true;
  r = syncSplitFiles(r.saved, ["a", "b", "c", "d"], isOpen);
  assert.deepEqual(r.set, { d: false });
  apply(r.set);
  // c leaves the grid (unticked): it gets its own state back at once.
  r = syncSplitFiles(r.saved, ["a", "b", "d"], isOpen);
  assert.deepEqual(r.set, { c: true });
  apply(r.set);
  // Leave Split: everyone back to how they were before Split.
  r = syncSplitFiles(r.saved, [], isOpen);
  apply(r.set);
  assert.deepEqual(r.saved, {});
  assert.deepEqual(open, { a: true, b: false, c: true, d: true });
  // Nothing to do when nothing changes.
  assert.deepEqual(syncSplitFiles({}, [], isOpen).set, {});
});

test("Files state carried across a pop-out (saved state travels with the tab)", () => {
  // Main: a was open before Split; popping out hands { a: true } to the new window.
  const carried = { a: true };
  const open: Record<string, boolean> = { a: false };   // arrives collapsed
  let r = syncSplitFiles(carried, ["a"], (id) => !!open[id]);
  assert.deepEqual(r.set, {}, "not re-recorded as closed");
  r = syncSplitFiles(r.saved, [], (id) => !!open[id]);
  assert.deepEqual(r.set, { a: true });
});

test("Files toggle for all panes, broadcast count", () => {
  assert.equal(filesAllTarget([false, false]), true);
  assert.equal(filesAllTarget([false, true]), false);
  assert.equal(filesAllTarget([]), true);
  assert.equal(broadcastCount(["a", "b"], "c"), 3);
  assert.equal(broadcastCount(["a", "b"], "a"), 2);
  assert.equal(broadcastCount([], null), 0);
});
