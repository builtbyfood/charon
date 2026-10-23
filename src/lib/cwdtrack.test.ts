import { test } from "node:test";
import assert from "node:assert/strict";
import xtermHeadless from "@xterm/headless";
import { attachOsc7 } from "./cwdtrack.ts";
import { followTarget } from "./panecwd.ts";
import { osc7ToLocal } from "./paths.ts";

const { Terminal } = xtermHeadless as unknown as typeof import("@xterm/headless");
const write = (t: InstanceType<typeof Terminal>, s: string) => new Promise<void>((r) => t.write(s, r));

// The whole "→ cwd" chain on the real xterm parser: an OSC 7 in the output
// updates the tab's cwd, tells the backend (session_set_cwd), and gives the
// pane's follow effect a target.
test("an OSC 7 sequence updates session cwd and triggers the follow target", async () => {
  const term = new Terminal({ allowProposedApi: true });
  let cwd: string | undefined = "/home/admin";
  const backend: string[] = [];
  attachOsc7(term, () => cwd, (dir) => { cwd = dir; backend.push(dir); });

  // What bash's precmd prints after `cd /tmp`: 133;D then OSC 7, BEL-terminated,
  // split across two writes the way the coalescer may deliver it.
  await write(term, "\x1b]133;D;0\x07\x1b]7;file://super-earth/t");
  await write(term, "mp\x07admin@super-earth:/tmp$ ");
  assert.equal(cwd, "/tmp");
  assert.deepEqual(backend, ["/tmp"]);

  // The pane last followed /home/admin → it must navigate to /tmp.
  assert.equal(followTarget(cwd, "/home/admin", false, osc7ToLocal), "/tmp");

  // Same directory again (another prompt in /tmp) → no new change, no follow.
  await write(term, "\x1b]7;file://super-earth/tmp\x07");
  assert.deepEqual(backend, ["/tmp"]);
  assert.equal(followTarget(cwd, "/tmp", false, osc7ToLocal), null);

  // ESC \ terminator and a Windows (pwsh) path on a local tab.
  await write(term, "\x1b]7;file://WIN/C:/Users/travis\x1b\\");
  assert.equal(cwd, "/C:/Users/travis");
  assert.equal(followTarget(cwd, "/tmp", true, osc7ToLocal), "C:\\Users\\travis");
  term.dispose();
});
