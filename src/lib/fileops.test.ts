import { test } from "node:test";
import assert from "node:assert/strict";
import { canPaste, collisions, dropAllowed, isWithin, parentOf, type FileClip } from "./fileops.ts";

test("isWithin: self and descendants only", () => {
  assert.ok(isWithin("/a/b", "/a/b"));
  assert.ok(isWithin("/a/b", "/a/b/c"));
  assert.ok(isWithin("/a/b/", "/a/b/c"));
  assert.ok(!isWithin("/a/b", "/a/bc"));
  assert.ok(!isWithin("/a/b", "/a"));
  assert.ok(isWithin("/", "/x"));
  assert.ok(isWithin("C:\\x", "C:\\x\\y"));
});

test("parentOf spells the pane's separators", () => {
  assert.equal(parentOf("/a/b"), "/a");
  assert.equal(parentOf("/a"), "/");
  assert.equal(parentOf("/a/b/"), "/a");
  assert.equal(parentOf("C:\\x\\y"), "C:\\x");
  assert.equal(parentOf("C:\\x"), "C:\\");
});

test("dropAllowed refuses self, own subtree, and a no-op move", () => {
  const items = ["/home/u/proj", "/home/u/a.txt"];
  assert.ok(dropAllowed(items, "/home/u/other", "move"));
  assert.ok(!dropAllowed(items, "/home/u/proj", "move"), "onto itself");
  assert.ok(!dropAllowed(items, "/home/u/proj/sub", "copy"), "into its own subtree");
  assert.ok(!dropAllowed(items, "/home/u", "move"), "already there");
  assert.ok(dropAllowed(items, "/home/u", "copy"), "copy in place = duplicate");
  assert.ok(dropAllowed(items, "/home", "move"), "up to an ancestor");
  assert.ok(!dropAllowed([], "/x", "move"));
});

test("canPaste needs the same session and backend", () => {
  // A fresh clip per call: canPaste is a type guard, so asserts would narrow a shared one.
  const clip = (paths = ["/a"]): FileClip | null => ({ op: "copy", session: "s1", via: "sftp", paths });
  assert.ok(canPaste(clip(), { session: "s1", via: "sftp" }));
  assert.ok(!canPaste(clip(), { session: "s2", via: "sftp" }));
  assert.ok(!canPaste(clip(), { session: "s1", via: "sudo" }));
  assert.ok(!canPaste(null, { session: "s1", via: "sftp" }));
  assert.ok(!canPaste(clip([]), { session: "s1", via: "sftp" }));
});

test("collisions", () => {
  assert.deepEqual(collisions(["a", "b"], ["b", "c"]), ["b"]);
  assert.deepEqual(collisions(["a"], []), []);
});
