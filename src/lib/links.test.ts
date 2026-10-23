import { test } from "node:test";
import assert from "node:assert/strict";
import { findLinks, parseOsc52, trimUrl, type Row } from "./links.ts";

/** Rows for a `width`-column terminal; a leading "\u0000" marks a soft-wrapped row. */
function rows(...lines: string[]): Row[] {
  return lines.map((l) => {
    const wrapped = l.startsWith("\u0000");
    const text = wrapped ? l.slice(1) : l;
    return { text, cols: Array.from(text, (_, i) => i), wrapped };
  });
}
const urls = (r: Row[], w: number) => findLinks(r, w).map((l) => l.url);

test("a plain URL on one row, trailing punctuation dropped", () => {
  const l = findLinks(rows("see https://example.com/a?b=1. ok"), 80);
  assert.equal(l.length, 1);
  assert.equal(l[0].url, "https://example.com/a?b=1");
  assert.deepEqual(l[0].start, { row: 0, col: 4 });
  assert.deepEqual(l[0].end, { row: 0, col: 28 });
});

test("two URLs on a row, and parentheses", () => {
  assert.deepEqual(urls(rows("(http://a.io/x) and https://en.wikipedia.org/wiki/Foo_(bar)"), 80),
    ["http://a.io/x", "https://en.wikipedia.org/wiki/Foo_(bar)"]);
});

test("a soft-wrapped URL continues in column 0", () => {
  const w = 20;
  const l = findLinks(rows("go: https://exampl", "\u0000e.com/very/long", "next"), w);
  assert.equal(l.length, 1);
  assert.equal(l[0].url, "https://example.com/very/long");
  assert.deepEqual(l[0].end, { row: 1, col: 14 });
});

test("a URL hard-broken at the right edge and indented on the next row (Claude Code)", () => {
  const w = 30;
  //                 0         1         2         3
  //                 012345678901234567890123456789
  const r = rows("⏺ Docs: https://docs.example.c",
                 "  om/en/setup?tab=linux",
                 "  More text here.");
  const l = findLinks(r, w);
  assert.equal(l.length, 1);
  assert.equal(l[0].url, "https://docs.example.com/en/setup?tab=linux");
  assert.deepEqual(l[0].start, { row: 0, col: 8 });
  assert.deepEqual(l[0].end, { row: 1, col: 22 });
});

test("a URL inside a bordered box joins across the border", () => {
  const w = 24;
  const r = rows("│ https://a.example/xyz │", "│ /more/path          │");
  assert.deepEqual(urls(r, w), ["https://a.example/xyz/more/path"]);
});

test("a URL that ends short of the edge is not joined with the next row", () => {
  assert.deepEqual(urls(rows("https://a.io/x", "continues here"), 80), ["https://a.io/x"]);
});

test("a URL at the edge followed by a new URL stays two links", () => {
  const w = 16;
  assert.deepEqual(urls(rows("https://a.io/abc", "https://b.io/x"), w), ["https://a.io/abc", "https://b.io/x"]);
});

test("wide characters map to their cell columns", () => {
  // "表" takes two cells: the URL starts in column 3, not 2.
  const r: Row[] = [{ text: "表 https://x.io/p", cols: [0, 2, ...Array.from("https://x.io/p", (_, i) => 3 + i)], wrapped: false }];
  assert.deepEqual(findLinks(r, 80)[0].start, { row: 0, col: 3 });
});

test("trimUrl keeps balanced parentheses", () => {
  assert.equal(trimUrl("https://x.io/a_(b)"), "https://x.io/a_(b)");
  assert.equal(trimUrl("https://x.io/a)"), "https://x.io/a");
  assert.equal(trimUrl("https://x.io/a)."), "https://x.io/a");
});

test("OSC 52 sets the clipboard; queries and junk are ignored", () => {
  const b64 = Buffer.from("héllo\nworld", "utf8").toString("base64");
  assert.equal(parseOsc52(`c;${b64}`), "héllo\nworld");
  assert.equal(parseOsc52(`;${b64}`), "héllo\nworld");
  assert.equal(parseOsc52("c;?"), null, "never answer a clipboard read");
  assert.equal(parseOsc52("c;"), null);
  assert.equal(parseOsc52("nonsense"), null);
});
