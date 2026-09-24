#!/usr/bin/env node
// Single source of truth for the app version: src-tauri/tauri.conf.json (the file
// Tauri itself reads). package.json and Cargo.toml are made to follow it, so the
// three can never drift. The UI shows the version via app_info, which returns
// CARGO_PKG_VERSION — kept equal to tauri.conf.json by this script.
//
// Usage:
//   node scripts/bump-version.mjs            # propagate tauri.conf.json's version
//   node scripts/bump-version.mjs 0.3.0      # set that version everywhere
//   npm run bump -- 0.3.0
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tauriConfPath = join(root, "src-tauri", "tauri.conf.json");
const pkgPath = join(root, "package.json");
const cargoPath = join(root, "src-tauri", "Cargo.toml");

const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

const arg = process.argv[2];
if (arg && !SEMVER.test(arg)) {
  console.error(`bump-version: "${arg}" is not a valid semver (e.g. 0.3.0)`);
  process.exit(1);
}

// tauri.conf.json is canonical. If a version was passed, write it there first.
const tauriConf = JSON.parse(readFileSync(tauriConfPath, "utf8"));
if (arg) {
  tauriConf.version = arg;
  writeFileSync(tauriConfPath, JSON.stringify(tauriConf, null, 2) + "\n");
}
const version = tauriConf.version;
if (!SEMVER.test(version)) {
  console.error(`bump-version: tauri.conf.json version "${version}" is not valid semver`);
  process.exit(1);
}

// package.json — keep every other field byte-for-byte; only the version changes.
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
const pkgWas = pkg.version;
pkg.version = version;
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");

// Cargo.toml — rewrite only the first `version = "..."` (the [package] one).
const cargo = readFileSync(cargoPath, "utf8");
let cargoWas = null;
const cargoNext = cargo.replace(/^version = "([^"]*)"/m, (_m, v) => {
  cargoWas = v;
  return `version = "${version}"`;
});
if (cargoWas === null) {
  console.error("bump-version: could not find `version = \"...\"` in Cargo.toml");
  process.exit(1);
}
writeFileSync(cargoPath, cargoNext);

console.log(`version → ${version}`);
console.log(`  tauri.conf.json ${arg ? "(set, canonical)" : "(source)"}`);
console.log(`  package.json    ${pkgWas} → ${version}`);
console.log(`  Cargo.toml      ${cargoWas} → ${version}`);
console.log("Note: run a build so Cargo.lock records the new package version.");
