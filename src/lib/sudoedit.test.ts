import { test } from "node:test";
import assert from "node:assert/strict";
import { canRetryAsSudo, ensureSudo, isPermissionError } from "./sudoedit.ts";
import type { SudoOutcome } from "./api.ts";

const remote = { fs: "sess-1", via: "sftp" as const, path: "/etc/hosts" };

test("retry as sudo triggers only on EACCES", () => {
  assert.ok(canRetryAsSudo(remote, "open /etc/hosts.charon-tmp: Permission denied"));
  assert.ok(canRetryAsSudo(remote, new Error("EACCES")));
  assert.ok(canRetryAsSudo({ ...remote, via: "shell" }, "Permission denied"), "a non-root shell backend can retry too");
  // Any other failure: no offer.
  for (const e of ["No such file", "connection lost", "disk full (ENOSPC)", "is 70000000 bytes; the editor limit is 67108864"])
    assert.ok(!canRetryAsSudo(remote, e), e);
  // Already sudo, a local file, or untitled: nothing to retry with.
  assert.ok(!canRetryAsSudo({ ...remote, via: "sudo" }, "Permission denied"));
  assert.ok(!canRetryAsSudo({ ...remote, fs: null }, "Permission denied"));
  assert.ok(!canRetryAsSudo({ ...remote, path: null }, "Permission denied"));
  assert.ok(isPermissionError("permission DENIED"));
});

const okOutcome: SudoOutcome = { ok: true, probe: "/usr/lib/openssh/sftp-server", sudo: "ok", reason: "" };

test("ensureSudo: stored password or live channel → no ask", async () => {
  let asked = 0;
  await ensureSudo("s", { open: async () => okOutcome, ask: async () => { asked++; return "pw"; } });
  assert.equal(asked, 0);
});

test("ensureSudo: nothing stored → asks once, then opens with that password", async () => {
  const calls: (string | undefined)[] = [];
  await ensureSudo("s", {
    open: async (_s, pw) => { calls.push(pw); if (!pw) throw "a sudo password is required"; return okOutcome; },
    ask: async () => "hunter2",
  });
  assert.deepEqual(calls, [undefined, "hunter2"]);
});

test("ensureSudo: MCP (no ask) and refusals surface as errors", async () => {
  await assert.rejects(ensureSudo("s", { open: async () => { throw "a sudo password is required"; } }), /password is required/);
  await assert.rejects(ensureSudo("s", { open: async () => ({ ok: false, probe: null, sudo: "refused", reason: "wrong password" }) }), /refused: wrong password/);
  await assert.rejects(ensureSudo("s", { open: async () => { throw "a sudo password is required"; }, ask: async () => null }), /cancelled/);
});
