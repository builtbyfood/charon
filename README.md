# Charon

One tool for engineers who live in terminals: connection manager, SSH/serial/local
terminals, an editor, sanitised copy/paste, and an MCP server so agents can drive
real sessions. Portable on Windows, `.deb`/`.rpm`/AppImage on Linux, one codebase.

Named for the ferryman — it gets you across to the other machine.

## Stack

| Layer      | Choice                                  | Why |
|------------|------------------------------------------|-----|
| Shell      | Tauri 2 (Rust)                           | ~10 MB binaries, native bundling for exe/deb/rpm/AppImage |
| Frontend   | Svelte 5 + Vite + TypeScript             | Small, fast, no runtime bloat |
| Terminal   | xterm.js + fit/webgl/search addons       | Battle-tested; cell grid makes clean copy trivial |
| Editor     | Monaco (phase 2)                         | Notepad++ replacement, remote edit over SFTP |
| SSH/SFTP   | `russh` + `russh-sftp`                   | Pure Rust, no OpenSSL packaging pain |
| Local PTY  | `portable-pty`                           | cmd / pwsh / WSL / bash / zsh |
| Serial     | `serialport`                             | Console cables |
| Secrets    | `keyring` (installed) / `age` (portable) | Never plaintext on disk |
| Store      | SQLite via `rusqlite` (bundled)          | Connection tree, credentials refs, history |
| MCP        | `rmcp` (phase 3)                         | Agents call `run_command`, `read_scrollback`, `sftp_*` |

## Layout

```
src-tauri/          Rust backend (Tauri commands, SSH, PTY, profiles, portable mode)
src/                Svelte frontend (sidebar tree, terminal tabs)
shell-integration/  OSC 133 hooks for bash/zsh/fish/PowerShell
profiles/           Example profile + connection tree in TOML
docs/               Architecture and phase plan
.github/workflows/  Build matrix: Windows portable exe, Linux deb/rpm/AppImage
```

## Build

Prereqs: Rust stable, Node 20+, and Tauri 2 platform deps
(<https://v2.tauri.app/start/prerequisites/>). On Debian/Ubuntu:

```
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Then:

```
npm install
npm run tauri dev          # dev loop
npm run tauri build        # bundles for the current OS
```

Bundles land in `src-tauri/target/release/bundle/`.

## Portable mode

Drop an empty file named `portable.flag` next to the executable. Charon then keeps
everything (`charon.db`, `profiles/`, `vault.age`, logs, and on Windows the
WebView2 profile under `webview/`) in `./data/` beside the binary and never
touches AppData / `~/.config`. The Windows CI artefact ships
this way by default, and `npm run portable` builds the same thing locally
(`dist-portable/charon.exe` + `portable.flag` on Windows, a tarball on Linux).
The bar at the bottom of the window says which mode is active and where the data
lives; `npm run tauri dev` is always installed mode.

## Status

Phase 1 skeleton. See `docs/ARCHITECTURE.md` for the phase plan and what's stubbed.
Nothing here has been compiled yet against a full Tauri toolchain — expect to fix
a handful of version/API mismatches on first `cargo build`.
