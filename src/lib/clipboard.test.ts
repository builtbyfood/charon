import { test } from "node:test";
import assert from "node:assert/strict";
import { pasteWarnings, cleanForCopy } from "./clipboard.ts";

test("a plain one-line paste warns nothing (sent immediately, no guard)", () => {
  assert.deepEqual(pasteWarnings("sudo virsh list --all", 2), []);
  // A one-liner copied with a trailing newline is still one line, not two.
  assert.deepEqual(pasteWarnings("sudo virsh list --all\n", 2), []);
  assert.deepEqual(pasteWarnings("one line\r\n", 2), []);
});

test("the line warning engages at the threshold, counting real content lines", () => {
  assert.deepEqual(pasteWarnings("a\nb", 2), ["2 lines"]);
  assert.deepEqual(pasteWarnings("a\nb\nc\n", 2), ["3 lines"]); // trailing newline ignored
  // A higher threshold suppresses small multi-line pastes.
  assert.deepEqual(pasteWarnings("a\nb", 3), []);
  assert.deepEqual(pasteWarnings("a\nb\nc", 3), ["3 lines"]);
});

test("hidden characters and $-prefixed doc lines always warn, even one line", () => {
  assert.ok(pasteWarnings("clean​text", 2).some((w) => w.includes("zero-width")));
  assert.deepEqual(pasteWarnings("$ apt update", 2), ["lines start with '$ ' (doc snippet?)"]);
});

test("cleanForCopy strips zero-width and normalises newlines", () => {
  assert.equal(cleanForCopy("a​b"), "ab");
  assert.equal(cleanForCopy("a\r\nb"), "a\nb");
});
