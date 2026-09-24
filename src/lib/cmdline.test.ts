import { test } from "node:test";
import assert from "node:assert/strict";
import { cmdContext, currentCommand } from "./cmdline.ts";

test("cmdContext finds the command and the option token", () => {
  assert.deepEqual(cmdContext("ls -"), { command: "ls", token: "-" });
  assert.deepEqual(cmdContext("ls --"), { command: "ls", token: "--" });
  assert.deepEqual(cmdContext("git log --onel"), { command: "git", token: "--onel" });
  assert.deepEqual(cmdContext("sudo systemctl --n"), { command: "systemctl", token: "--n" });
  assert.deepEqual(cmdContext("FOO=1 env BAR=2 tar -x"), { command: "tar", token: "-x" });
  // The reported repro: sudo is skipped, the whole `--all` token is the prefix.
  assert.deepEqual(cmdContext("sudo virsh --all"), { command: "virsh", token: "--all" });
});

test("cmdContext returns null when not on a flag", () => {
  assert.equal(cmdContext("ls "), null);
  assert.equal(cmdContext("ls foo"), null);
  assert.equal(cmdContext(""), null);
  assert.equal(cmdContext("-x"), null);          // no command yet
  assert.equal(cmdContext("echo hi | -"), null); // pipeline noise
});

test("currentCommand finds the command being typed", () => {
  assert.equal(currentCommand("tar"), "tar");
  assert.equal(currentCommand("ls -la /etc"), "ls");
  assert.equal(currentCommand("sudo systemctl restart ng"), "systemctl");
  assert.equal(currentCommand("cat a | grep foo"), "grep");
  assert.equal(currentCommand(""), null);
  assert.equal(currentCommand("-x"), null);
  assert.equal(currentCommand("./run.sh"), null);
});
