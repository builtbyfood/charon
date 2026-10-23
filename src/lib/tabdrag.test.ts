import { test } from "node:test";
import assert from "node:assert/strict";
import { dropIndex, reorder } from "./tabdrag.ts";

const rects = [{ left: 0, right: 100 }, { left: 100, right: 200 }, { left: 200, right: 300 }];

test("drop index is decided by the tab midpoints", () => {
  assert.equal(dropIndex(rects, -50), 0);
  assert.equal(dropIndex(rects, 49), 0);
  assert.equal(dropIndex(rects, 51), 1);
  assert.equal(dropIndex(rects, 260), 3);
  assert.equal(dropIndex([], 10), 0);
});

test("reorder moves one tab and keeps the rest in order", () => {
  const l = ["a", "b", "c", "d"].map((id) => ({ id }));
  const ids = (x: { id: string }[]) => x.map((t) => t.id).join("");
  assert.equal(ids(reorder(l, "a", 2)), "bcad");
  assert.equal(ids(reorder(l, "d", 0)), "dabc");
  assert.equal(ids(reorder(l, "b", 99)), "acdb");
  assert.equal(reorder(l, "b", 1), l, "no move → same array (no reactive churn)");
  assert.equal(reorder(l, "zz", 0), l);
});
