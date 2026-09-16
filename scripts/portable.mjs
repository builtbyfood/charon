// `npm run portable`: release build, then a portable bundle you can run from a
// USB stick — the exe beside an empty `portable.flag`, so Charon keeps its data
// in ./data/ next to it (see src-tauri/src/paths.rs). Windows → dist-portable/
// (charon.exe + portable.flag); Linux → dist-portable/charon-portable-linux-<arch>.tar.gz.
// Same layout as the CI artefact in .github/workflows/build.yml.
import { execSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const out = join(root, "dist-portable");
const win = process.platform === "win32";
const exe = join(root, "src-tauri", "target", "release", win ? "charon.exe" : "charon");

const run = (cmd) => { console.log(`> ${cmd}`); execSync(cmd, { cwd: root, stdio: "inherit" }); };

// --no-bundle: just the release binary; installers are `npm run tauri build`'s job.
run("npm run tauri build -- --no-bundle");
if (!existsSync(exe)) { console.error(`build did not produce ${exe}`); process.exit(1); }

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

if (win) {
  cpSync(exe, join(out, "charon.exe"));
  writeFileSync(join(out, "portable.flag"), "");
  console.log(`\nportable build: ${join(out, "charon.exe")} (+ portable.flag; data lands in ${join(out, "data")} on first run)`);
} else {
  const stage = join(out, "charon");
  mkdirSync(stage, { recursive: true });
  cpSync(exe, join(stage, "charon"));
  writeFileSync(join(stage, "portable.flag"), "");
  const tar = `charon-portable-linux-${process.arch}.tar.gz`;
  run(`tar -C "${out}" -czf "${join(out, tar)}" charon`);
  rmSync(stage, { recursive: true, force: true });
  console.log(`\nportable build: ${join(out, tar)} (extract, run ./charon/charon; needs webkit2gtk-4.1 on the host)`);
}
