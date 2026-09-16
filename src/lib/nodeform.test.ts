// Run with `npm test` (node:test, TypeScript stripped natively by Node ≥ 22.18).
import { test } from "node:test";
import assert from "node:assert/strict";
import { formFromSettings, settingsFromForm } from "./nodeform.ts";
import type { Settings } from "./api.ts";

/** What the store hands the UI for a folder with nothing set: every key present, every value null. */
const STORED_EMPTY = JSON.parse(
  '{"host":null,"port":null,"user":null,"credential":null,"jump_host":null,"color":null,"tags":null,"startup":null,' +
  '"shell_integration":null,"mcp_tier":null,"shell":null,"cwd":null,"serial":null}',
) as Settings;

test("editing a folder without touching anything keeps every field on inherit", () => {
  const r = settingsFromForm(formFromSettings(STORED_EMPTY), "folder");
  assert.ok(r.ok);
  for (const [k, v] of Object.entries(r.settings)) assert.equal(v, undefined, `${k} flipped from inherit to ${JSON.stringify(v)}`);
  // JSON.stringify drops undefined, so nothing reaches the backend as an explicit value.
  assert.equal(JSON.stringify(r.settings), "{}");
});

test("null tri-states are not stringified into the select values", () => {
  const f = formFromSettings(STORED_EMPTY);
  assert.equal(f.shellInt, "");
  assert.equal(f.mcp, "");
  assert.equal(f.port, "");
  assert.equal(f.color, "");
  assert.equal(f.startup, "");
});

test("explicit values survive a round trip", () => {
  const s: Settings = {
    host: "h", port: 2222, user: "u", credential: "c", jump_host: "j", color: "#abcdef",
    tags: ["a", "b"], startup: ["uptime", "ls"], shell_integration: false, mcp_tier: "trusted",
  };
  const r = settingsFromForm(formFromSettings(s), "ssh");
  assert.ok(r.ok);
  assert.deepEqual(r.settings, { ...s, shell: undefined, cwd: undefined, serial: undefined });
  const on = settingsFromForm(formFromSettings({ shell_integration: true }), "folder");
  assert.ok(on.ok); assert.equal(on.settings.shell_integration, true);
});

test("validation", () => {
  const f = formFromSettings({});
  assert.deepEqual(settingsFromForm({ ...f, port: "70000" }, "ssh"), { ok: false, error: "port must be 1–65535" });
  assert.deepEqual(settingsFromForm(f, "serial"), { ok: false, error: "serial port is required" });
  const ser = settingsFromForm({ ...f, serialPort: "COM3", baud: "115200" }, "serial");
  assert.ok(ser.ok);
  assert.deepEqual(ser.settings.serial, { port: "COM3", baud: 115200, data_bits: 8, parity: "none", stop_bits: 1 });
});
