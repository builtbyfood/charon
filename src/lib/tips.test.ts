import { test } from "node:test";
import assert from "node:assert/strict";
import { parseTips } from "./tips.ts";

test("parseTips reads tip/note pairs and skips comments", () => {
  const y = [
    "# a comment",
    '- tip: "!!"',
    '  note: "repeat the last command"',
    "- tip: cd -",
    "  note: previous directory",
    "",
    "- tip: 'Ctrl+R'",
  ].join("\n");
  const t = parseTips(y);
  assert.equal(t.length, 3);
  assert.deepEqual(t[0], { tip: "!!", note: "repeat the last command" });
  assert.deepEqual(t[1], { tip: "cd -", note: "previous directory" });
  assert.deepEqual(t[2], { tip: "Ctrl+R", note: "" });
});
