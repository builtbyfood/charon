import { test } from "node:test";
import assert from "node:assert/strict";
import { fmtStamp, gutterRows, stampIndex, type Stamp } from "./stamps.ts";

const at = (h: number, m: number, s: number) => new Date(2026, 9, 21, h, m, s).getTime();
const NOW = at(23, 0, 0);

test("a line takes the newest stamp at or above it", () => {
  const st: Stamp[] = [{ line: 2, t: 1 }, { line: 5, t: 2 }, { line: 9, t: 3 }];
  assert.equal(stampIndex(st, 0), -1);
  assert.equal(stampIndex(st, 2), 0);
  assert.equal(stampIndex(st, 4), 0);
  assert.equal(stampIndex(st, 5), 1);
  assert.equal(stampIndex(st, 100), 2);
  assert.equal(stampIndex([], 3), -1);
});

test("times are HH:MM:SS today, with the date otherwise", () => {
  assert.equal(fmtStamp(at(9, 5, 7), NOW), "09:05:07");
  assert.equal(fmtStamp(new Date(2026, 9, 20, 23, 59, 59).getTime(), NOW), "10/20 23:59:59");
});

test("gutter: text rows get a time, wraps and blanks do not, repeats are flagged", () => {
  const lines: Record<number, { text: string; wrapped: boolean }> = {
    10: { text: "$ make", wrapped: false },
    11: { text: "cc -o a a.c", wrapped: false },
    12: { text: "continued", wrapped: true },
    13: { text: "", wrapped: false },
    14: { text: "done", wrapped: false },
  };
  const st: Stamp[] = [{ line: 10, t: at(12, 0, 0) }, { line: 14, t: at(12, 0, 5) }];
  const g = gutterRows(st, 10, 6, (l) => lines[l], NOW);
  assert.deepEqual(g.map((r) => r.text), ["12:00:00", "12:00:00", "", "", "12:00:05", ""]);
  assert.deepEqual(g.map((r) => r.repeat), [false, true, false, false, false, false]);
});
