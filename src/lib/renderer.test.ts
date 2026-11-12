import { test } from "node:test";
import assert from "node:assert/strict";
import { isLinuxWebview, rendererSetting, resolveRenderer } from "./renderer.ts";

test("renderer: auto is DOM on Linux, WebGL elsewhere; a named one is kept", () => {
  assert.equal(resolveRenderer("auto", true), "dom");
  assert.equal(resolveRenderer("auto", false), "webgl");
  assert.equal(resolveRenderer("webgl", true), "webgl", "WebGL stays available on Linux, opt-in");
  assert.equal(resolveRenderer("canvas", false), "canvas");
  assert.equal(rendererSetting(undefined), "auto");
  assert.equal(rendererSetting("nonsense"), "auto");
  assert.equal(rendererSetting("dom"), "dom");
  assert.ok(isLinuxWebview("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)"));
  assert.ok(!isLinuxWebview("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130"));
  assert.ok(!isLinuxWebview("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"));
});
