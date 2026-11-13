// Records what an interactive bash 5 echoes for the bash hook injection, into
// src/lib/fixtures/bash-inject-echo.json, for the "giving up" test in inject.test.ts.
// The test reads the recording instead of a live shell: runners' bash differs
// (macOS ships 3.2, whose line editing echoes it differently).
// Re-run after changing shell-integration/bash.sh or posixInjection:
//   node scripts/record-inject-echo.ts [path to a bash >= 5]
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { posixInjection } from "../src/lib/inject.ts";

const bashPath = process.argv[2] ?? "bash";
const version = spawnSync(bashPath, ["-c", "echo $BASH_VERSION"], { encoding: "utf8" }).stdout.trim();
if (!/^[5-9]\./.test(version)) {
  console.error(`need bash >= 5, ${bashPath} is ${version || "missing"}`);
  process.exit(1);
}
const hook = readFileSync(new URL("../shell-integration/bash.sh", import.meta.url), "utf8");
const injection = posixInjection("bash", hook);
// Interactive bash echoes typed input on stderr: `> ` continuation prompts and readline's `\r` redraws.
const r = spawnSync(bashPath, ["--norc", "--noprofile", "-i"], { input: injection, encoding: "utf8", env: { ...process.env, PS1: "$ " } });
const all = r.stderr.split("\n").filter((l) => !/job control|terminal process group/.test(l)).join("\n");
const end = all.lastIndexOf("esac");
if (end < 0 || !/\r/.test(all.slice(0, end))) {
  console.error("no echo with redraws captured:", JSON.stringify(all.slice(0, 200)));
  process.exit(1);
}
const echo = all.slice(0, end + 4); // the echo only, not the hooked prompt after it
mkdirSync(new URL("../src/lib/fixtures/", import.meta.url), { recursive: true });
const out = new URL("../src/lib/fixtures/bash-inject-echo.json", import.meta.url);
writeFileSync(out, JSON.stringify({ bash: version, columns: 80, injection, echo }, null, 1) + "\n");
console.log(`recorded ${echo.length} chars of echo from bash ${version}`);
