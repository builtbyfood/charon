import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { INJECT_END, INJECT_SKIP, InjectionSwallower, SWALLOW_CAP_MS, SWALLOW_IDLE_MS, posixInjection, withoutEcho } from "./inject.ts";

const bashHook = readFileSync(new URL("../../shell-integration/bash.sh", import.meta.url), "utf8");
const zshHook = readFileSync(new URL("../../shell-integration/zsh.zsh", import.meta.url), "utf8");
const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

/** Feed `input` to an interactive bash as if typed; stdout, and bash's own error lines (its stderr also carries the echo). */
function bash(input: string): { out: string; err: string } | undefined {
  const r = spawnSync("bash", ["--norc", "--noprofile", "-i"], { input, encoding: "utf8", env: { ...process.env, PS1: "$ ", CHARON_HOOKED: "" } });
  if (r.error) return undefined;
  const errors = r.stderr.split(/[\r\n]+/).filter((l) => /^bash: /.test(l) && !/no job control|terminal process group|initialize_job_control/.test(l));
  return { out: r.stdout, err: errors.join("\n") };
}

test("posix injection: one guarded line per shell, history-skipping space", () => {
  const b = posixInjection("bash", bashHook);
  assert.ok(b.startsWith(' case "${BASH_VERSION-}" in "") '), b.slice(0, 60));
  assert.ok(b.includes("CHARON_EOF\n)\"; printf '\\033]7770;end\\a';; esac\n"), b.slice(-80));
  assert.ok(posixInjection("zsh", zshHook).startsWith(' case "${ZSH_VERSION-}" in'));
  assert.ok(!b.includes("\n#"), "comments stripped");
});

test("bash runs the hook and prints the end marker", (t) => {
  const r = bash(posixInjection("bash", bashHook) + "echo \"pc=$PROMPT_COMMAND\"\n");
  if (!r) return t.skip("no bash");
  assert.ok(r.out.includes(INJECT_END) && !r.out.includes(INJECT_SKIP), JSON.stringify(r.out));
  assert.match(r.out, /pc=__charon_in_prompt=1; __charon_precmd;/);
  assert.equal(r.err, "");
});

test("a shell that isn't the hook's prints the skip marker and installs nothing", (t) => {
  // bash has no $ZSH_VERSION: it stands in for BusyBox ash / dash meeting a hook.
  const r = bash(posixInjection("zsh", zshHook) + "echo \"pc=[$PROMPT_COMMAND] hooked=[$CHARON_HOOKED]\"\n");
  if (!r) return t.skip("no bash");
  assert.ok(r.out.includes(INJECT_SKIP) && !r.out.includes(INJECT_END), JSON.stringify(r.out));
  assert.match(r.out, /pc=\[\] hooked=\[\]/);
  assert.equal(r.err, "", "no error output");
});

test("injecting twice: no `return` error, prompt hook installed once", (t) => {
  const inj = posixInjection("bash", bashHook);
  const r = bash(inj + inj + "echo \"pc=$PROMPT_COMMAND\"\n");
  if (!r) return t.skip("no bash");
  assert.equal(r.err, "");
  assert.equal(r.out.split("__charon_precmd;").length - 1, 1, JSON.stringify(r.out));
});

test("swallower: ends on either marker, split across chunks, and says which", () => {
  const s = new InjectionSwallower();
  s.start("", 0);
  assert.equal(s.filter(enc("echo of the hook " + INJECT_SKIP.slice(0, 5)), 1).length, 0);
  assert.equal(s.result, undefined);
  const shown = dec(s.filter(enc(INJECT_SKIP.slice(5) + "$ "), 2));
  assert.equal(shown, "\r\x1b[2K$ ");
  assert.equal(s.result, "skip");
  assert.equal(s.swallowing, false);
  s.start("", 0);
  s.filter(enc("x" + INJECT_END + "y"), 1);
  assert.equal(s.result, "end");
});

test("swallower: inactivity ends it, a busy echo keeps it going up to the cap", () => {
  const s = new InjectionSwallower();
  s.start("x", 0);
  assert.equal(s.expire(10_000), undefined, "before the first byte only the cap counts");
  s.filter(enc("e"), 10_000);
  assert.equal(s.expire(10_000 + SWALLOW_IDLE_MS - 1), undefined);
  for (let t = 11_000; t < SWALLOW_CAP_MS; t += 1000) assert.equal(s.filter(enc("e"), t).length, 0, `still echoing at ${t}`);
  assert.ok(s.expire(SWALLOW_CAP_MS) !== undefined, "capped");
  assert.equal(s.result, "timeout");
  assert.equal(s.swallowing, false);

  s.start("x", 0);
  s.filter(enc("e"), 100);
  assert.ok(s.expire(100 + SWALLOW_IDLE_MS) !== undefined, "idle");
  // A chunk arriving after the idle deadline (timer not yet run) is shown, with what was held.
  s.start(" echo-of-injection", 0);
  s.filter(enc("echo-of-injection"), 100);
  assert.equal(dec(s.filter(enc("$ "), 100 + SWALLOW_IDLE_MS)), "$ ");
});

/** A recorded bash 5 echo of the bash hook injection (scripts/record-inject-echo.ts). */
const recorded: { bash: string; injection: string; echo: string } = JSON.parse(
  readFileSync(new URL("./fixtures/bash-inject-echo.json", import.meta.url), "utf8"),
);

test("giving up shows what isn't the echo, not the whole echo", () => {
  // What an interactive bash 5 at 80 columns really echoes for it: `> ` continuation
  // prompts and readline's `\r` redraws (`<<'C\rCHARON_EOF'`, the "&&turturn" effect).
  // Recorded, not live: the runner's bash may be 3.2 (macOS), which echoes it differently.
  const { injection: inj, echo } = recorded;
  assert.match(echo, /\r/, "the echo has redraws");
  const s = new InjectionSwallower();
  s.start(inj, 0);
  s.filter(enc(echo.replace(/\n/g, "\r\n") + "\r\nash: eval: line 1: syntax error\r\nbox:~# "), 1);
  const shown = dec(s.expire(1 + SWALLOW_IDLE_MS)!);
  assert.equal(shown, "\r\x1b[2Kash: eval: line 1: syntax error\r\nbox:~# ");
  assert.equal(dec(withoutEcho(enc("plain output\r\n"), inj)), "plain output\r\n");
});

test("the recorded echo is of the current bash injection", () => {
  assert.equal(
    recorded.injection,
    posixInjection("bash", bashHook),
    "shell-integration/bash.sh or posixInjection changed: re-run `node scripts/record-inject-echo.ts` with a bash >= 5",
  );
});
