import { test } from "node:test";
import assert from "node:assert/strict";
import { forwardConsole } from "./diag.ts";

test("only [charon] lines are forwarded, with objects stringified and log as info", () => {
  const saved = { info: console.info, log: console.log, warn: console.warn, error: console.error };
  for (const k of Object.keys(saved) as (keyof typeof saved)[]) console[k] = () => {};
  const sent: string[] = [];
  try {
    forwardConsole((level, msg) => sent.push(`${level} ${msg}`));
    console.info("[charon] connecting", { id: "a" });
    console.log("[charon] raf: on");
    console.warn("unrelated");
    console.error("[charon] clipboard: read failed", new Error("boom"));
  } finally {
    Object.assign(console, saved);
  }
  assert.deepEqual(sent, ['info [charon] connecting {"id":"a"}', "info [charon] raf: on", "error [charon] clipboard: read failed boom"]);
});
