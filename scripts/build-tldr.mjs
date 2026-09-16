// Build the bundled tldr asset: download tldr-pages, keep English
// common+linux+windows, and write a gzipped JSON map { name: markdown } to
// src-tauri/resources/tldr.json.gz. Run manually or in CI to refresh; the app
// ships the result and can also fetch a fresh copy into its data dir at runtime.
//
//   node scripts/build-tldr.mjs
//
// Needs network. ~7000 pages compress to ~1 MB.
import { createWriteStream } from "node:fs";
import { mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";

const URL = "https://github.com/tldr-pages/tldr/releases/latest/download/tldr.zip";
const root = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const work = join(tmpdir(), "charon-tldr");
const platforms = ["common", "linux", "windows"];

async function main() {
  await rm(work, { recursive: true, force: true });
  await mkdir(work, { recursive: true });
  const zip = join(work, "tldr.zip");

  console.log("downloading", URL);
  const res = await fetch(URL);
  if (!res.ok) throw new Error(`download failed: ${res.status}`);
  await new Promise((ok, err) => {
    const out = createWriteStream(zip);
    res.body.pipeTo(new WritableStream({ write: (c) => out.write(c), close: () => out.end(ok), abort: err })).catch(err);
  });

  // Use the system unzip (present on the CI images and dev boxes here).
  const pat = platforms.map((p) => `pages/${p}/*`);
  execFileSync("unzip", ["-q", "-o", zip, ...pat, "-d", work], { stdio: "inherit" });

  const pages = {};
  for (const platform of platforms) {
    const dir = join(work, "pages", platform);
    let files;
    try { files = await readdir(dir); } catch { continue; }
    for (const f of files) {
      if (!f.endsWith(".md")) continue;
      const name = f.slice(0, -3);
      // common wins over platform-specific only if not already present; linux/windows override common.
      const md = await readFile(join(dir, f), "utf8");
      if (platform === "common" && pages[name]) continue;
      pages[name] = md;
    }
  }

  const outDir = join(root, "public");
  await mkdir(outDir, { recursive: true });
  const json = JSON.stringify(pages);
  const gz = gzipSync(Buffer.from(json), { level: 9 });
  const outPath = join(outDir, "tldr.json.gz");
  await writeFile(outPath, gz);
  console.log(`wrote ${outPath}: ${Object.keys(pages).length} pages, ${(gz.length / 1024).toFixed(0)} KiB gzipped`);
}

main().catch((e) => { console.error(e); process.exit(1); });
