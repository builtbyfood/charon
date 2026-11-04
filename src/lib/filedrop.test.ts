import { test } from "node:test";
import assert from "node:assert/strict";
import { platformOf, toCssPoint, pickTarget } from "./filedrop.ts";

test("platform from the webview user agent", () => {
  assert.equal(platformOf("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130.0"), "windows");
  assert.equal(platformOf("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)"), "linux");
  assert.equal(platformOf("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"), "macos");
});

test("only Windows drop positions are physical pixels", () => {
  assert.deepEqual(toCssPoint({ x: 1500, y: 600 }, "windows", 1.5), { x: 1000, y: 400 });
  // GTK hands over widget (CSS) pixels: a HiDPI scale must not shrink them.
  assert.deepEqual(toCssPoint({ x: 1000, y: 400 }, "linux", 2), { x: 1000, y: 400 });
  assert.deepEqual(toCssPoint({ x: 1000, y: 400 }, "macos", 2), { x: 1000, y: 400 });
  assert.deepEqual(toCssPoint({ x: 300, y: 200 }, "windows", 0), { x: 300, y: 200 });
});

test("the drop goes to the zone under the pointer, never to another zone", () => {
  const paneA = {}, paneB = {};
  const editor = { zone: "editor" };
  const a = { zone: "files", el: paneA }, b = { zone: "files", el: paneB };
  const all = [editor, a, b];
  assert.equal(pickTarget(all, "files", paneB), b);
  assert.equal(pickTarget(all, "files", paneA), a);
  assert.equal(pickTarget(all, "editor", {}), editor);
  assert.equal(pickTarget(all, null, null), null);
  // A file pane nobody registered (yet) does not fall through to the editor.
  assert.equal(pickTarget(all, "files", {}), null);
});
