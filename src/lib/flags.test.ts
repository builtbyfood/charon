import { test } from "node:test";
import assert from "node:assert/strict";
import { flagsFromHelp, flagsFromTldr, mergeFlags, matchFlags, flagAccept, runFlagAccept, PickerGate, isSingleKeypress, type Flag } from "./flags.ts";
import { cmdContext } from "./cmdline.ts";

test("flagsFromHelp parses common --help layouts", () => {
  const help = [
    "Usage: ls [OPTION]... [FILE]...",
    "",
    "  -a, --all                  do not ignore entries starting with .",
    "  -l                         use a long listing format",
    "      --color[=WHEN]         colorize the output",
    "  -w, --width=COLS",
    "                             set output width to COLS",
    "  --help     display this help and exit",
  ].join("\n");
  const f = flagsFromHelp(help);
  const byShort = (s: string) => f.find((x) => x.names.includes(s));
  assert.deepEqual(byShort("-a")!.names, ["-a", "--all"]);
  assert.match(byShort("-a")!.desc!, /do not ignore/);
  assert.equal(byShort("-l")!.desc, "use a long listing format");
  assert.deepEqual(byShort("--color")!.names, ["--color"]);
  assert.deepEqual(byShort("-w")!.names, ["-w", "--width"]);
  assert.match(byShort("-w")!.desc!, /set output width/); // wrapped description
  assert.ok(byShort("--help"));
});

test("flagsFromTldr pulls flags from example code, tagged with the example text", () => {
  const md = [
    "# tar",
    "",
    "- Extract an archive:",
    "",
    "`tar -xf {{archive.tar}}`",
    "",
    "- Create a gzipped archive:",
    "",
    "`tar -czf {{target.tar.gz}} {{file}}`",
  ].join("\n");
  const f = flagsFromTldr(md);
  const names = f.flatMap((x) => x.names);
  assert.ok(names.includes("-xf") || names.includes("-x")); // token as written
  assert.ok(f.some((x) => x.from === "tldr" && /archive/i.test(x.desc ?? "")));
});

test("mergeFlags prefers help descriptions and collapses aliases", () => {
  const help = flagsFromHelp("  -f, --force        overwrite without asking");
  const tldr = flagsFromTldr("- Force it:\n\n`rm -f {{file}}`");
  const merged = mergeFlags(tldr, help);
  const force = merged.find((x) => x.names.includes("-f"))!;
  assert.equal(force.from, "help");
  assert.equal(force.desc, "overwrite without asking");
  // -f and --force are one entry, not two.
  assert.equal(merged.filter((x) => x.names.includes("-f") || x.names.includes("--force")).length, 1);
});

test("matchFlags filters by typed prefix", () => {
  const flags = mergeFlags(flagsFromHelp([
    "  -a, --all       x",
    "  -l              y",
    "      --almost    z",
  ].join("\n")));
  assert.deepEqual(matchFlags(flags, "--al").flatMap((f) => f.names).sort(), ["--all", "--almost", "-a"].sort());
  assert.equal(matchFlags(flags, "--").every((f) => f.names.some((n) => n.startsWith("--"))), true);
  assert.deepEqual(matchFlags(flags, "-l").flatMap((f) => f.names), ["-l"]);
});

test("flagAccept: case 1 — insert only the missing suffix", () => {
  assert.deepEqual(flagAccept("--al", ["--all"]), { insert: "l", passthrough: false });
  assert.deepEqual(flagAccept("--", ["--all"]), { insert: "all", passthrough: false });
  // The stale-token bug: a `-` prefix must never yield a `-all` corrupting send —
  // it inserts only the difference to the chosen long flag.
  assert.deepEqual(flagAccept("-", ["--all"]), { insert: "-all", passthrough: false });
});

test("flagAccept: case 2 — exact prefix passes through to run", () => {
  assert.deepEqual(flagAccept("--all", ["--all"]), { insert: "", passthrough: true });
  assert.deepEqual(flagAccept("-a", ["-a", "--all"]), { insert: "", passthrough: true });
});

test("flagAccept: case 3 — a non-matching highlight never corrupts, passes through", () => {
  assert.deepEqual(flagAccept("--xyz", ["--all"]), { insert: "", passthrough: true });
  assert.deepEqual(flagAccept("--all", ["--long"]), { insert: "", passthrough: true });
});

test("runFlagAccept: mouse and keyboard accept send the identical suffix and refocus", () => {
  const flags = [
    { names: ["--all"], desc: "list all (running & inactive)", from: "help" as const },
    { names: ["--color"], desc: "colorize (auto|always|never)", from: "help" as const },
  ];
  const spy = () => {
    const calls = { sent: [] as string[], focus: 0, close: 0 };
    return { calls, h: { send: (s: string) => calls.sent.push(s), focus: () => calls.focus++, close: () => calls.close++ } };
  };
  // Keyboard: Enter on the highlighted row (index 0), prefix "--al".
  const kb = spy();
  const rk = runFlagAccept("--al", flags, 0, kb.h);
  // Mouse: click that same row, same live prefix.
  const ms = spy();
  const rm = runFlagAccept("--al", flags, 0, ms.h);

  assert.deepEqual(kb.calls.sent, ["l"]);
  assert.deepEqual(ms.calls.sent, kb.calls.sent);   // identical sent=
  assert.equal(rk.insert, rm.insert);
  assert.equal(rk.consumed, rm.consumed);
  assert.ok(kb.calls.focus >= 1 && ms.calls.focus >= 1); // xterm refocused afterwards
  assert.ok(kb.calls.close >= 1 && ms.calls.close >= 1); // popup dismissed
});

test("runFlagAccept sends only the suffix — never the description or row text", () => {
  const flags = [{ names: ["--color"], desc: "colorize output (auto, always, never)", from: "help" as const }];
  const sent: string[] = [];
  runFlagAccept("--co", flags, 0, { send: (s) => sent.push(s), focus() {}, close() {} });
  assert.deepEqual(sent, ["lor"]);
  const joined = sent.join("");
  assert.ok(!joined.includes("("), "must not send the description's '('");
  assert.ok(!joined.includes(",") && !joined.includes(" "), "must not send the row's name list / spaces");
});

test("runFlagAccept passes through (no send) on an exact match, still closes + refocuses", () => {
  const flags = [{ names: ["--all"], from: "help" as const }];
  const calls = { sent: [] as string[], focus: 0, close: 0 };
  const r = runFlagAccept("--all", flags, 0, { send: (s) => calls.sent.push(s), focus: () => calls.focus++, close: () => calls.close++ });
  assert.equal(r.consumed, false);
  assert.deepEqual(calls.sent, []);
  assert.ok(calls.focus >= 1 && calls.close >= 1);
});

// A minimal shell + Terminal.svelte picker loop: the shell echoes input, runs a
// line on CR/LF and prints a fresh prompt (133;B → gate.prompt()); after every
// echo the picker evaluates (the debounced cursor-move hook); a typed Enter is
// steered by the popup exactly as attachCustomKeyEventHandler does.
function harness(flags: Flag[], gated: boolean) {
  const gate = new PickerGate();
  const ran: string[] = [];
  const logs: string[] = [];
  let line = "";
  let popup: { token: string; list: Flag[] } | null = null;
  const evalPicker = () => {
    const b = gate.blocked();
    if (gated && b) { if (gate.shouldLog()) logs.push(`[charon] flags: suppressed reason=${b}`); popup = null; return; }
    const ctx = cmdContext(line);
    popup = ctx ? { token: ctx.token, list: matchFlags(flags, ctx.token) } : null;
  };
  const shell = (bytes: string) => {
    for (const ch of bytes.replace(/\x1b\[20[01]~/g, "")) {
      if (ch === "\r" || ch === "\n") { ran.push(line); line = ""; gate.prompt(); }
      else line += ch;
    }
    evalPicker();
  };
  return {
    ran, logs, get popup() { return popup; }, get line() { return line; },
    /** pasteToSession → writePaste (bracketed, pasteRun=auto keeps no trailing \n here). */
    paste(text: string) {
      if (gated) { gate.pasteBegin(); popup = null; }
      gate.send(() => gate.input(text));   // term.paste → onData, synchronous
      shell(text);                           // the shell's echo/prompts arrive later
    },
    /** A typed key: the key handler first, then (if not swallowed) onData. */
    key(d: string) {
      if (d === "\r" && popup) {
        const r = runFlagAccept(popup.token, popup.list, 0, { send: (s) => { line += s; }, focus: () => {}, close: () => { popup = null; } });
        if (r.consumed) return;            // Enter swallowed by the popup
      }
      gate.input(d);
      shell(d);
    },
  };
}
// Any list whose top match for `--all` isn't `--all` itself makes Enter insert, not run.
const ALL_FLAGS: Flag[] = [{ names: ["--all-files"], from: "help" }, { names: ["-a", "--all"], from: "help" }];

test("paste ending in a flag: without the gate the popup opens and eats Enter (the bug)", () => {
  const h = harness(ALL_FLAGS, false);
  h.paste("cd /tmp\nls --all");
  assert.ok(h.popup, "popup opened on pasted text");
  h.key("\r");
  assert.deepEqual(h.ran, ["cd /tmp"], "last line did not run");
});

test("paste text ending in --all → no popup, all lines execute", () => {
  const h = harness(ALL_FLAGS, true);
  h.paste("cd /tmp\nls --all");
  assert.equal(h.popup, null);
  h.key("\r");
  assert.deepEqual(h.ran, ["cd /tmp", "ls --all"]);
  assert.equal(h.popup, null);
  assert.deepEqual(h.logs, ["[charon] flags: suppressed reason=paste"]);
});

test("paste closes an already-open popup, and typing after the next prompt re-enables it", () => {
  const h = harness(ALL_FLAGS, true);
  for (const c of "ls --a") h.key(c);
  assert.ok(h.popup, "typed flag opens the picker");
  h.paste("ll\n");
  assert.equal(h.popup, null);
  for (const c of "ls -") h.key(c);
  assert.ok(h.popup, "prompt after the pasted newline ended suppression; typed keys evaluate");
});

test("pasted line without newline stays suppressed for typed keys until the next prompt", () => {
  const h = harness(ALL_FLAGS, true);
  h.paste("ls");
  for (const c of " --a") h.key(c);
  assert.equal(h.popup, null);
  h.key("\r");
  for (const c of "ls --a") h.key(c);
  assert.ok(h.popup);
});

test("cancelled paste lifts suppression; the gate ignores the paste's own echo", () => {
  const g = new PickerGate();
  g.pasteBegin();
  g.send(() => g.input("x"));               // single-char paste echo is not a keypress
  g.prompt();
  assert.equal(g.blocked(), "paste");
  g.input("a");
  assert.equal(g.blocked(), null);
  g.pasteBegin(); g.pasteCancelled();
  assert.equal(g.blocked(), "paste");       // last input was the paste…
  g.input("-");
  assert.equal(g.blocked(), null);          // …until the user types
  g.input("bulk text from an IME");
  assert.equal(g.blocked(), "bulk");
});

test("isSingleKeypress", () => {
  for (const k of ["a", "-", "\r", "\x7f", "😀", "\x1b[A", "\x1bOA", "\x1b[3~", "\x1b[1;5C", "\x1bb"]) assert.ok(isSingleKeypress(k), JSON.stringify(k));
  for (const k of ["ls", "cd /tmp\n", "\x1b[200~x\x1b[201~"]) assert.ok(!isSingleKeypress(k), JSON.stringify(k));
});
