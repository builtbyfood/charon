import { test } from "node:test";
import assert from "node:assert/strict";
import { joinPath, basename, crumbsOf, osc7ToLocal, humanSize, modeString, sq } from "./paths.ts";

test("joinPath spells separators per filesystem", () => {
  assert.equal(joinPath("/home/admin", "x"), "/home/admin/x");
  assert.equal(joinPath("/", "x"), "/x");
  assert.equal(joinPath("C:\\Users", "x"), "C:\\Users\\x");
  assert.equal(joinPath("C:\\", "x"), "C:\\x");
  assert.equal(basename("/home/admin/a.txt"), "a.txt");
  assert.equal(basename("C:\\Users\\a.txt"), "a.txt");
});

test("crumbs", () => {
  assert.deepEqual(crumbsOf("/home/admin"), [
    { label: "/", path: "/" }, { label: "home", path: "/home" }, { label: "admin", path: "/home/admin" },
  ]);
  assert.deepEqual(crumbsOf("/"), [{ label: "/", path: "/" }]);
  assert.deepEqual(crumbsOf("C:\\Users\\travis"), [
    { label: "C:\\", path: "C:\\" }, { label: "Users", path: "C:\\Users" }, { label: "travis", path: "C:\\Users\\travis" },
  ]);
});

test("osc7ToLocal undoes the pwsh hook's /C:/ spelling", () => {
  assert.equal(osc7ToLocal("/C:/Users/travis"), "C:\\Users\\travis");
  assert.equal(osc7ToLocal("/C:"), "C:\\");
  assert.equal(osc7ToLocal("/home/x"), "/home/x");
});

test("sizes and modes", () => {
  assert.equal(humanSize(0), "0 B");
  assert.equal(humanSize(1536), "1.5 KiB");
  assert.equal(humanSize(200 * 1024 * 1024), "200 MiB");
  assert.equal(modeString(0o755, true), "drwxr-xr-x");
  assert.equal(modeString(0o644), "-rw-r--r--");
  assert.equal(modeString(0o4755), "-rwsr-xr-x");
  assert.equal(modeString(0o1777, true), "drwxrwxrwt");
});

test("sq quotes for sh", () => {
  assert.equal(sq("/home/admin"), "'/home/admin'");
  assert.equal(sq("it's here"), "'it'\\''s here'");
});
