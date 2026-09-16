import { test } from "node:test";
import assert from "node:assert/strict";
import { detectShell, looksLikeShellPrompt, parseOsc7 } from "./prompt.ts";

test("detectShell recognises the four shells and nothing else", () => {
  assert.equal(detectShell("admin@super-earth:~$ "), "bash");
  assert.equal(detectShell("root@super-earth:~# "), "bash");
  assert.equal(detectShell("[travis@box ~]$ "), "bash");
  assert.equal(detectShell("box% "), "zsh");
  assert.equal(detectShell("travis@box ~> "), "fish");
  assert.equal(detectShell("PS C:\\Users\\travis> "), "pwsh");
  assert.equal(detectShell("PS /home/travis> "), "pwsh");
  assert.equal(detectShell("Password: "), undefined);
  assert.equal(detectShell(">>> "), undefined);
  assert.equal(detectShell("Last login: Mon Sep 14 21:03:43 2026"), undefined);
});

test("looksLikeShellPrompt accepts sudo/su prompts and rejects REPLs", () => {
  assert.equal(looksLikeShellPrompt("root@super-earth:~# "), "bash");
  assert.equal(looksLikeShellPrompt("[root@box ~]# "), "bash");
  assert.equal(looksLikeShellPrompt("# "), "bash");
  assert.equal(looksLikeShellPrompt("$ "), "bash");
  assert.equal(looksLikeShellPrompt("root@box ~# "), "bash");
  // things that end in # or $ but are not shells
  assert.equal(looksLikeShellPrompt("postgres=# "), undefined);
  assert.equal(looksLikeShellPrompt("sqlite> "), undefined);
  assert.equal(looksLikeShellPrompt("mysql> "), undefined);
  assert.equal(looksLikeShellPrompt("price is 5$"), undefined);
  assert.equal(looksLikeShellPrompt("sh-5.1# "), undefined);
});

test("parseOsc7 strips the host and decodes percent escapes", () => {
  assert.equal(parseOsc7("file://super-earth/home/admin"), "/home/admin");
  assert.equal(parseOsc7("file://localhost/"), "/");
  assert.equal(parseOsc7("file:///tmp/x y"), "/tmp/x y");
  assert.equal(parseOsc7("file://h/a%20b/%E2%9C%93"), "/a b/✓");
  assert.equal(parseOsc7("file://WIN/C:/Users/travis"), "/C:/Users/travis");
  assert.equal(parseOsc7("file://h/100%"), "/100%"); // bad escape: raw
  assert.equal(parseOsc7("garbage"), undefined);
});
