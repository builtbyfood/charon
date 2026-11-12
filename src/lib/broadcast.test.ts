import { test } from "node:test";
import assert from "node:assert/strict";
import { broadcastTargets, fanOut, fanSudo, isTerminalReport, tailLine } from "./broadcast.ts";
import { looksLikeSudoPrompt } from "./prompt.ts";

const tabs = [
  { id: "a", session: "s-a" },
  { id: "ed", session: null },        // editor tab: no session
  { id: "b", session: "s-b" },
  { id: "c", session: "s-c" },
  { id: "d", session: "s-d" },
];

test("targets: selected tabs in strip order, never the active one, only live sessions", () => {
  const sel = new Set(["d", "a", "ed", "c"]);
  assert.deepEqual(broadcastTargets(tabs, sel, "c").map((t) => t.id), ["a", "d"]);
  // The active tab need not be selected: it is the source either way.
  assert.deepEqual(broadcastTargets(tabs, sel, "b").map((t) => t.id), ["a", "c", "d"]);
  assert.deepEqual(broadcastTargets(tabs, new Set(), "a"), []);
  // Two tabs on one session (cannot happen today) still get one write.
  assert.deepEqual(broadcastTargets([...tabs, { id: "e", session: "s-a" }], new Set(["a", "e"]), "b").map((t) => t.id), ["a"]);
});

test("fan-out: every session gets the data in order, a failure doesn't stop the rest", async () => {
  const log: string[] = [];
  const errs = await fanOut(["s-a", "s-b", "s-c"], "rm -i x\r", (s, d) => {
    log.push(`${s}:${d}`);
    if (s === "s-b") throw new Error("closed");
  });
  assert.deepEqual(log, ["s-a:rm -i x\r", "s-b:rm -i x\r", "s-c:rm -i x\r"]);
  assert.deepEqual(errs, [{ session: "s-b", error: "Error: closed" }]);
  // Async writes are all started before any is awaited, in order.
  const started: string[] = [];
  const done = fanOut(["1", "2", "3"], "k", (s) => { started.push(s); return new Promise((r) => setTimeout(r, 5)); });
  assert.deepEqual(started, ["1", "2", "3"]);
  assert.deepEqual(await done, []);
  // A rejected promise is reported, not thrown.
  assert.deepEqual(await fanOut(["x"], "k", () => Promise.reject("gone")), [{ session: "x", error: "gone" }]);
});

test("terminal reports stay with the active tab; keys are broadcast", () => {
  for (const r of ["\x1b[?1;2c", "\x1b[>0;276;0c", "\x1b[24;80R", "\x1b[0n", "\x1b[I", "\x1b[O", "\x1b[<0;10;5M", "\x1b[<0;10;5m",
    "\x1b[M #!", "\x1b]11;rgb:0000/0000/0000\x1b\\", "\x1bP1$r0m\x1b\\", "\x1b[?2004;1$y", "\x1b[8;24;80t"]) {
    assert.ok(isTerminalReport(r), JSON.stringify(r));
  }
  for (const k of ["l", "s", "\r", "\x7f", "\x03", "\x1b[A", "\x1bOA", "\x1b[15~", "\x1bb", "ls -la", "\t", "\x1b"]) {
    assert.ok(!isTerminalReport(k), JSON.stringify(k));
  }
});

test("hidden target's last line: escapes, CR overwrite, newlines", () => {
  let l = tailLine("", "ok\r\n\x1b[01;32muser@box\x1b[0m:~$ sudo ls\r\n");
  assert.equal(l, "");
  l = tailLine(l, "[sudo] password for ");
  l = tailLine(l, "user: ");
  assert.equal(l, "[sudo] password for user: ");
  assert.ok(looksLikeSudoPrompt(l));
  assert.ok(!looksLikeSudoPrompt(tailLine(l, "\r\nfile\r\nuser@box:~$ ")));
  assert.equal(tailLine("", "abcdef\rXY"), "XYcdef");
  assert.equal(tailLine("", "\x1b]0;title\x07$ "), "$ ");
});

test("sudo fan-out: each prompted session answers with its own credential", async () => {
  const stored: Record<string, string> = { "s-a": "pw-a", "s-b": "pw-b", "s-c": "pw-c" };
  const typed: Record<string, string> = {};
  // Stands in for sudo_send_password: the backend looks the password up by the session it is given.
  const send = async (session: string) => { if (!stored[session]) return false; typed[session] = stored[session]; return true; };
  const r = await fanSudo([
    { session: "s-a", prompted: true },
    { session: "s-b", prompted: false },   // at a shell prompt: must not get a password
    { session: "s-c", prompted: true },
    { session: "s-x", prompted: true },    // no stored password
  ], send);
  assert.deepEqual(typed, { "s-a": "pw-a", "s-c": "pw-c" });
  assert.deepEqual(r, [{ session: "s-a", sent: true }, { session: "s-b", sent: false }, { session: "s-c", sent: true }, { session: "s-x", sent: false }]);
  // One failing send doesn't stop the others.
  const r2 = await fanSudo([{ session: "1", prompted: true }, { session: "2", prompted: true }], async (s) => { if (s === "1") throw new Error("x"); return true; });
  assert.deepEqual(r2, [{ session: "1", sent: false }, { session: "2", sent: true }]);
});

// ---- typed input reaching every target (origin routing + ordered writes)
import { fansOut } from "./broadcast.ts";
import { OrderedWriter } from "./writeq.ts";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * The source as Terminal.svelte drives it: every onData chunk goes to its own
 * session, and to the targets when `fansOut` says so. The fake backend finishes
 * each write after a random 0–8 ms, so unordered writes would arrive shuffled.
 */
async function typeInto(keys: string[], intervalMs: number, opts: { replies?: Record<number, string> } = {}) {
  const got: Record<string, string> = { src: "", t1: "", t2: "", t3: "", t4: "" };
  const writer = new OrderedWriter(async (s, d) => { await sleep(Math.random() * 8); got[s] += d; });
  const targets = ["t1", "t2", "t3", "t4"];
  const onData = (d: string, pasting = false) => {
    void writer.send("src", d);
    if (fansOut(d, pasting)) void fanOut(targets, d, (s, x) => writer.send(s, x));
  };
  for (let i = 0; i < keys.length; i++) {
    onData(keys[i]);
    if (opts.replies?.[i]) onData(opts.replies[i]);   // xterm answering a query mid-line
    if (intervalMs) await sleep(intervalMs);
  }
  await sleep(20);
  await writer.idle();
  return got;
}

for (const interval of [1, 200]) {
  test(`20-character line typed at ${interval} ms reaches every target byte-identical, one \r per Enter`, { timeout: 20_000 }, async () => {
    const line = "echo testing 123 abc";
    assert.equal(line.length, 20);
    const got = await typeInto([...line, "\r"], interval, { replies: { 5: "\x1b[24;80R", 12: "\x1b[I" } });
    for (const t of ["t1", "t2", "t3", "t4"]) {
      assert.equal(got[t], line + "\r", `${t}: ${JSON.stringify(got[t])}`);
      assert.equal(got[t].split("\r").length - 1, 1, `${t}: exactly one \r`);
    }
    // The source also got xterm's replies, the targets didn't.
    assert.equal(got.src, "echo t\x1b[24;80Resting \x1b[I123 abc\r");
  });
}

test("routing is by origin: keys and picker inserts fan out, paste echo and replies don't", () => {
  for (const d of ["e", "\r", "\x7f", "\x1b[A", "--all", "\x03"]) assert.ok(fansOut(d, false), JSON.stringify(d));
  assert.ok(!fansOut("rm -rf /tmp/x\r", true), "a paste's own echo (it is fanned out separately, once)");
  assert.ok(!fansOut("\x1b[?1;2c", false));
  assert.ok(!fansOut("\x1b[<0;3;4M", false));
  assert.ok(!fansOut("", false));
});

test("ordered writer: in order per session, joined while one is in flight, a failure doesn't stall", async () => {
  const calls: string[] = [];
  let fail = true;
  const w = new OrderedWriter(async (s, d) => { await sleep(5); calls.push(`${s}:${d}`); if (fail && d === "a") { fail = false; throw new Error("x"); } });
  const p1 = w.send("s", "a");
  void w.send("s", "b"); void w.send("s", "c");
  void w.send("o", "z");
  await assert.rejects(p1);
  await w.idle();
  assert.deepEqual(calls.filter((c) => c.startsWith("s:")), ["s:a", "s:bc"]);
  assert.ok(calls.includes("o:z"));
});

test("ordered writer under load: 50 KB paste plus 2000 keystrokes, nothing reordered or lost, no stall", { timeout: 20_000 }, async () => {
  let got = "", calls = 0;
  const w = new OrderedWriter(async (_s, d) => { calls++; await sleep(Math.random() * 3); got += d; });
  let want = "";
  const paste = "x".repeat(50 * 1024);
  for (let i = 0; i < 2000; i++) {
    const k = String.fromCharCode(97 + (i % 26));
    void w.send("s", k); want += k;
    if (i === 700) { void w.send("s", paste); want += paste; }
  }
  const t0 = Date.now();
  await w.idle();
  assert.equal(got.length, want.length);
  assert.ok(got === want, "byte-identical, in order");
  assert.ok(calls < 100, `queued input is joined into few writes (${calls})`);
  assert.ok(Date.now() - t0 < 5000, "drains promptly");
});

import { DANGEROUS_DEFAULT, dangerousCommand, idleExpired, typedLine } from "./broadcast.ts";

test("dangerous prefixes: whole words, every command in a list, past sudo/env", () => {
  const d = (l: string) => dangerousCommand(l, DANGEROUS_DEFAULT);
  assert.equal(d("rm -rf /tmp/x"), "rm");
  assert.equal(d("sudo rm -rf /"), "rm");
  assert.equal(d("sudo -u root rm x"), "rm");
  assert.equal(d("cd /srv && rm -rf *"), "rm");
  assert.equal(d("ls; reboot"), "reboot");
  assert.equal(d("FOO=1 dd if=/dev/zero of=/dev/sda"), "dd");
  assert.equal(d("mkfs.ext4 /dev/sdb1"), "mkfs");
  assert.equal(d("/usr/sbin/shutdown -h now"), "shutdown");
  assert.equal(d("sudo systemctl stop nginx"), "systemctl stop");
  assert.equal(d("systemctl status nginx"), null);
  assert.equal(d("rmdir x"), null, "not a prefix of another word");
  assert.equal(d("echo rm"), null);
  assert.equal(d("grep reboot /var/log/syslog"), null);
  assert.equal(d(""), null);
  assert.equal(dangerousCommand("kubectl delete pod x", ["kubectl delete"]), "kubectl delete");
  assert.equal(dangerousCommand("rm x", []), null, "an empty list turns it off");
});

test("typed line: keys, backspace, clear, Enter", () => {
  let l = "";
  for (const k of ["r", "m", "x", "\x7f", " ", "-", "r", "f"]) l = typedLine(l, k);
  assert.equal(l, "rm -rf");
  assert.equal(typedLine(l, "\x1b[D"), "rm -rf", "arrows are skipped");
  assert.equal(typedLine(l, "\x15"), "");
  assert.equal(typedLine(l, "\r"), "");
  assert.equal(typedLine("", "ls\rrm"), "rm");
});

test("idle auto-off", () => {
  assert.equal(idleExpired(0, 10 * 60_000 - 1, 10), false);
  assert.equal(idleExpired(0, 10 * 60_000, 10), true);
  assert.equal(idleExpired(0, 1e12, 0), false, "0 = never");
});
