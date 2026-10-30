import { test } from "node:test";
import assert from "node:assert/strict";
import { promptUserOf } from "./prompt.ts";
import { followTarget, pushTarget } from "./panecwd.ts";
import { osc7ToLocal } from "./paths.ts";

test("promptUserOf reads the user, or nothing", () => {
  assert.equal(promptUserOf("admin@devbox:~$ "), "admin");
  assert.equal(promptUserOf("root@devbox:~# "), "root");
  assert.equal(promptUserOf("[root@box ~]# "), "root");
  assert.equal(promptUserOf("travis@box ~> "), "travis");
  assert.equal(promptUserOf("$ "), undefined);
  assert.equal(promptUserOf("PS C:\\Users\\travis> "), undefined);
  assert.equal(promptUserOf("postgres=# "), undefined);
});

test("followTarget only fires on a real cwd change (B3: no yank-back on manual nav)", () => {
  // Shell cwd unchanged since last follow → the pane's own navigation is not undone.
  assert.equal(followTarget("/home/admin", "/home/admin", false, osc7ToLocal), null);
  // Shell actually moved → follow.
  assert.equal(followTarget("/etc", "/home/admin", false, osc7ToLocal), "/etc");
  assert.equal(followTarget(undefined, undefined, false, osc7ToLocal), null);
  // Local: OSC 7 /C:/ form is converted.
  assert.equal(followTarget("/C:/Users/travis", undefined, true, osc7ToLocal), "C:\\Users\\travis");
});

test("pushTarget only fires on a real pane change (B3: guard the loop)", () => {
  assert.equal(pushTarget("/etc", "/etc"), null); // already cd'd here
  assert.equal(pushTarget("/etc", "/home/admin"), "/etc");
  assert.equal(pushTarget(undefined, undefined), null);
});
