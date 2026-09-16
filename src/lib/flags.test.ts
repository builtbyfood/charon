import { test } from "node:test";
import assert from "node:assert/strict";
import { flagsFromHelp, flagsFromTldr, mergeFlags, matchFlags } from "./flags.ts";

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
