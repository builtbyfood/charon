# Building Charon — Windows first, then Linux

Workflow: develop and test on Windows, push/copy the tree to a Linux box, build
deb/rpm/AppImage there. Never copy `node_modules/` or `src-tauri/target/` between
machines — they are platform-specific and huge. Git handles this for you via
`.gitignore`; if you tar instead, exclude them explicitly (see step 3).

Set `git config --global core.autocrlf input` on Windows before the first commit so
the shell hooks under `shell-integration/` don't pick up CRLF line endings.

---

## 1. Windows

Run from an elevated PowerShell.

```powershell
# Package manager (ships with Windows 10 1709+ / 11)
winget --version

# Rust + MSVC toolchain
winget install --id Rustlang.Rustup -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override `
  "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# Node 20 LTS
winget install --id OpenJS.NodeJS.LTS -e

# WebView2 runtime is preinstalled on Win11 and updated Win10; if missing:
winget install --id Microsoft.EdgeWebView2Runtime -e

# Optional but handy
winget install --id Git.Git -e
winget install --id 7zip.7zip -e
```

Close and reopen the terminal so PATH updates, then:

```powershell
rustup default stable-msvc
rustc --version; cargo --version; node --version; npm --version
```

Build and run:

```powershell
cd charon
npm install
npm run tauri dev             # dev loop with hot reload
npm run tauri build           # NSIS installer -> src-tauri\target\release\bundle\nsis\
```

NSIS is downloaded automatically by the Tauri CLI on first bundle; no manual install.

Portable build (no installer):

```powershell
npm run portable              # -> dist-portable\charon.exe + portable.flag
```

(That is `cargo build --release` plus copying the exe beside an empty `portable.flag`;
Charon then keeps its data in `dist-portable\data\`.)

Smoke test the portable exe — release builds behave differently from `tauri dev`
in one way that matters: keyboard focus. Type in the first modal before anything else:

```powershell
scripts\smoke-portable.ps1      # launches dist-portable\charon.exe, types into the
                                # Create-vault field, screenshots the window to
                                # dist-portable\smoke.png (expect dots in the field)
```

Manual version: start `dist-portable\charon.exe` from Explorer, do **not** click
anywhere, and type into the master-password field. Dots must appear.

Do not run two Charons at different integrity levels on one WebView2 profile: a
`tauri dev` started from an *elevated* terminal plus a double-clicked exe sharing
`%LOCALAPPDATA%\dev.builtbyfood.charon\EBWebView` renders fine but the
non-elevated one gets no keyboard input (UIPI blocks focus into the elevated
browser process). Portable and debug builds use their own profile directory
(`data\webview`, `webview-dev`) so this only bites two installed release copies.

Tests:

```powershell
cargo test --manifest-path src-tauri\Cargo.toml
```

### Windows gotchas
- If `cargo build` complains about `link.exe`, the VC Tools workload didn't install:
  open Visual Studio Installer → Modify → tick "Desktop development with C++".
- Long path errors under `target\`: `git config --system core.longpaths true` and
  enable long paths in Group Policy or `reg add HKLM\SYSTEM\CurrentControlSet\Control\FileSystem /v LongPathsEnabled /t REG_DWORD /d 1`.
- `rusqlite` bundled feature compiles SQLite from C — needs the MSVC toolchain above; nothing else.

---

## 2. Linux

Tauri 2 needs WebKitGTK 4.1 headers plus the usual build chain.

### Debian / Ubuntu 22.04+

```bash
sudo apt update
sudo apt install -y build-essential curl wget file pkg-config git \
  libwebkit2gtk-4.1-dev libssl-dev libxdo-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf rpm libudev-dev

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Node 20 via NodeSource
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
```

`rpm` lets the deb-based box also produce `.rpm` bundles. `libudev-dev` is for the
`serialport` crate.

### Fedora / RHEL 9 / Rocky / Alma

```bash
sudo dnf install -y @development-tools curl wget file git \
  webkit2gtk4.1-devel openssl-devel libxdo-devel libappindicator-gtk3-devel \
  librsvg2-devel patchelf rpm-build systemd-devel

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

sudo dnf install -y nodejs npm     # Fedora ships 20+; on RHEL 9: sudo dnf module enable nodejs:20 -y
```

On RHEL-family, `libxdo-devel` and `libappindicator-gtk3-devel` live in EPEL:
`sudo dnf install -y epel-release` first. `systemd-devel` provides libudev.

### Build

```bash
cd charon
npm ci                                   # uses package-lock.json from Windows
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --bundles deb,rpm,appimage
ls src-tauri/target/release/bundle/{deb,rpm,appimage}/
```

AppImage bundling downloads `linuxdeploy` on first run; it needs `file` and `patchelf`
present (installed above). If the box has no outbound internet, build deb/rpm only.

Install to test:

```bash
sudo apt install ./src-tauri/target/release/bundle/deb/charon_0.1.0_amd64.deb   # deb-based
sudo dnf install ./src-tauri/target/release/bundle/rpm/charon-0.1.0-1.x86_64.rpm  # rpm-based
```

Portable on Linux (tarball, no package manager):

```bash
mkdir -p portable && cp src-tauri/target/release/charon portable/ && touch portable/portable.flag
tar czf charon-linux-portable.tar.gz portable
```

### Verify on Linux (R2-6 checklist)
- Local PTY: open a **Local** tab → `$SHELL` (bash) with a prompt; 133 injects.
- SSH: connect a saved host; host-key TOFU prompt; **SFTP pane** (Ctrl+Shift+E)
  lists `~`; download/upload a file.
- Editor: open a remote file from the pane, edit, Ctrl+S, `cat` it back.
- Portable tarball: extract, run `./charon/charon`; the bar reads PORTABLE with
  the data dir under `./charon/data`.

### Linux gotchas
- "webkit2gtk-4.0 not found": you have an old distro (Ubuntu 20.04, Debian 11).
  Tauri 2 needs 4.1 — upgrade or build in a 22.04 container.
- Headless box: builds fine; only `tauri dev` needs a display.
- `EACCES` on `npm ci`: never run npm as root; fix ownership of `~/.npm`.
- **Vault**: installed mode uses the OS keyring via Secret Service — a running
  `gnome-keyring`/`kwallet` daemon (a desktop session, or `dbus-run-session`) is
  needed at runtime; headless/SSH sessions have none, so use **portable mode**
  (`vault.age` + master password) there.
- **WebGL**: on a software/headless GL stack the terminal probes for a WebGL2
  context and falls back to the DOM renderer automatically; force it with
  `renderer = "dom"` under `[appearance]` in `data/profiles/default.toml`.
- `npm test` needs Node ≥ 22.18 (native TypeScript for the `node:test` suites);
  the CI pins Node 22.

---

## 3. Moving the tree Windows → Linux

Preferred: push to GitHub from Windows, `git clone` on Linux. CI in
`.github/workflows/build.yml` then builds both platforms on every push anyway.

Manual copy, excluding the platform-specific dirs:

```powershell
# Windows side
tar -czf charon-src.tgz --exclude=node_modules --exclude=src-tauri/target --exclude=dist --exclude=.git charon
scp charon-src.tgz user@linuxbox:~
```

```bash
# Linux side
tar xzf charon-src.tgz && cd charon && npm ci
```

Keep `package-lock.json` in the copy so both sides resolve identical JS deps;
`Cargo.lock` likewise pins Rust crates.

### CI and releases

`.github/workflows/build.yml` runs on every push/PR: `npm run check`, `cargo
test`, `npm test`, then `tauri-action` bundles NSIS (Windows) and deb/rpm/AppImage
(Linux). Artefacts upload on every run; **pushing a `v*` tag** additionally drafts
a GitHub release with the installers, both bundles, and the portable zip/tarball
attached. Cut a release from a green `main`:

```bash
git tag v0.2.0 && git push origin v0.2.0
# → draft release "Charon v0.2.0" with nsis .exe, .deb, .rpm, .AppImage,
#   charon-windows-portable.zip, charon-linux-portable.tar.gz — review and publish.
```

The workflow needs no secrets beyond the automatic `GITHUB_TOKEN`.

---

## 4. Container build (optional, reproducible)

If the Linux box is a Fedora or RHEL desktop you don't want to litter with
`-devel` packages:

```bash
podman run --rm -it -v "$PWD":/src -w /src ubuntu:22.04 bash -c '
  apt update && apt install -y build-essential curl wget file pkg-config git \
    libwebkit2gtk-4.1-dev libssl-dev libxdo-dev libayatana-appindicator3-dev \
    librsvg2-dev patchelf rpm libudev-dev nodejs npm &&
  curl -sSf https://sh.rustup.rs | sh -s -- -y && . ~/.cargo/env &&
  npm ci && npm run tauri build -- --bundles deb,rpm'
```

AppImage generation inside a container needs `--privileged` or FUSE workarounds;
build deb/rpm in the container and AppImage on the host, or let CI do it.
