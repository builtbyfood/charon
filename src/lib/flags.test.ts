import { test } from "node:test";
import assert from "node:assert/strict";
import { flagsFromHelp, flagsFromTldr, mergeFlags, matchFlags, flagAccept, runFlagAccept } from "./flags.ts";

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
