import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampPane, PANE_DEFAULT, PANE_MIN, TERM_MIN, DIVIDER,
  colsFromProfile, visibleCols, gridTemplate, rowMinWidth, toggleCol, setColWidth, fitCol, DEFAULT_COLS,
} from "./filecols.ts";

test("pane width leaves both sides their minimum", () => {
  assert.equal(clampPane(500, 1600), 500);
  assert.equal(clampPane(100, 1600), PANE_MIN);
  assert.equal(clampPane(1500, 1600), 1600 - TERM_MIN - DIVIDER);
  // Not laid out yet: only the pane minimum applies.
  assert.equal(clampPane(900, 0), 900);
  // Too narrow for both: the pane keeps its minimum.
  assert.equal(clampPane(400, 400), PANE_MIN);
  assert.equal(clampPane(NaN, 1600), PANE_DEFAULT);
  assert.equal(clampPane(0, 1600), PANE_DEFAULT);
});

test("profile column settings are sanitised; Name is never hidden", () => {
  assert.deepEqual(colsFromProfile({ size: 90, bogus: 5, mtime: "x", mode: 10 }, ["name", "size", "size", "nope", 3]),
    { widths: { size: 90, mode: 50 }, hidden: ["size"] });
  assert.deepEqual(colsFromProfile(undefined, undefined), DEFAULT_COLS);
});

test("visible columns: hidden ones and Perm without mode bits are left out", () => {
  const keys = (l = DEFAULT_COLS, modes = true) => visibleCols(l, modes).map((c) => c.key).join(",");
  assert.equal(keys(), "name,size,mtime,mode");
  assert.equal(keys(DEFAULT_COLS, false), "name,size,mtime");
  assert.equal(keys({ widths: {}, hidden: ["mtime", "name"] }), "name,size,mode");
});

test("hiding toggles, but not for Name", () => {
  const l = toggleCol(DEFAULT_COLS, "size");
  assert.deepEqual(l.hidden, ["size"]);
  assert.deepEqual(toggleCol(l, "size").hidden, []);
  assert.equal(toggleCol(l, "name"), l);
});

test("grid: Name fills until it has a width of its own", () => {
  const cols = visibleCols(DEFAULT_COLS, true);
  assert.equal(gridTemplate(DEFAULT_COLS, cols), "minmax(80px, 1fr) 64px 118px 84px");
  const sized = setColWidth(setColWidth(DEFAULT_COLS, "name", 300), "size", 20);
  assert.equal(gridTemplate(sized, cols), "300px 40px 118px 84px");
  assert.equal(rowMinWidth(sized, cols), 300 + 40 + 118 + 84 + 3 * 6 + 16);
});

test("auto-fit takes the widest cell plus air, within the column's limits", () => {
  assert.equal(fitCol("name", [120.2, 340, 90]), 348);
  assert.equal(fitCol("size", [3]), 40);
  assert.equal(fitCol("mtime", []), 60);
  assert.equal(fitCol("name", [5000]), 1200);
});
