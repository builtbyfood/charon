# Changelog

All notable changes to Charon are recorded here. Versions follow semver; the
canonical version lives in `src-tauri/tauri.conf.json` and is propagated to
`package.json` and `Cargo.toml` by `npm run bump` (`scripts/bump-version.mjs`).

## 0.2.1

### Terminal
- **Flag picker ignores pastes** — pasting a block whose last line ends in a flag
  (`cd /tmp` ⏎ `ls --all`) no longer opens the popup and swallows the Enter that
  should run it; the picker only reacts to typed keystrokes.
- Flag picker: a mouse click accepts a flag (keeps terminal focus, inserts only the
  missing suffix).
- Fixed a crash on very large pastes and a doubled prompt on connect.
- Floating **Copy** button on a terminal selection.
- The paste guard only engages on real warnings, and its held-paste bar is easier
  to notice.

### Sessions
- SSH password logins fall back to keyboard-interactive auth when the server
  doesn't offer plain `password`.
- Local sessions can use a stored sudo credential; the sudo button appears only
  where sudo takes a tty password (not on Windows) and is detected reliably.
- Output coalescing always flushes the tail, is size-capped, and now also covers
  local PTY sessions.

### Platform / CI
- macOS builds both run on `macos-latest` (Intel cross-compiled; the `macos-13`
  image is retired); a tag creates one draft release that every job uploads into.
- `.gitattributes` forces LF checkouts, so CI-built Windows releases don't ship
  CRLF shell hooks.

## 0.2.0

### Files pane
- **Multi-select** — click, Shift+click (range), Ctrl/Cmd+click (toggle), Ctrl+A;
  the selection count shows in the pane header and the Download/Upload actions and
  right-click menu operate on the whole selection.
- **Recursive & multi-item transfers** — download/upload whole directory trees and
  multiple items with bounded concurrency (`files.parallel_transfers`, default 4),
  mtime/mode preservation, a follow-symlinks toggle, and unreadable entries skipped
  with a warning instead of aborting. Works across all backends (SFTP, sudo-sftp,
  ShellFs — the last via a `tar | base64` fast path). The transfers panel shows one
  row per top-level item (aggregate progress, expandable to per-file) with cancel
  and retry-failed-only, and a once-per-batch overwrite/skip/rename conflict prompt.

### Terminal
- **Paste reliability** — the paste guard is now an inline, non-modal bar (Enter/Esc)
  so the terminal never loses focus; `terminal.paste_run = "auto" | "wait"` controls
  whether a bracketed paste ending in a newline runs immediately; input queues (never
  drops) while a hidden command is capturing.
- **Sudo password button** — a `[sudo] password for <you>:` prompt shows a
  “send saved password” button (Ctrl+Shift+P) that sends the stored password without
  echo; `sudo.auto_send` auto-sends it (debounced) for the login user's own prompt.
- **Flag picker fix** — Enter now appends only the missing suffix (never a stray
  newline or a corrupting `-all`); an exact or non-matching token passes Enter through
  to run the line; it no longer opens on a mid-line token.

### Performance
- Output is coalesced in the backend to at most one UI event per 16 ms per session
  (SSH and local PTY), cutting IPC overhead under heavy output.
- Inactive tabs buffer output instead of running xterm's parser/renderer and release
  their WebGL context (recreated on show), so many tabs no longer thrash the GPU.
- A `[charon] perf …` line (events/s, bytes/s, write-queue depth, renderer) every 5 s
  for measuring throughput per tab.

### MCP
- `run_command` can auto-answer a sudo prompt, but only when tier-gated: never at
  **read**, via the approval dialog's separate “allow sudo password” opt-in at
  **exec-approve**, and via the node's `mcp_sudo` setting at **trusted**. The capture
  only answers the login user's own `[sudo] password for <user>:`, never a bare
  `Password:`.

### Platform / CI
- SSH keepalive (`ssh.keepalive_seconds`, default 30) replaces the idle-disconnect
  timeout; a `[closed — reconnect]` button appears on a real disconnect.
- macOS builds (arm64 + Intel) added to CI on every push/PR; `.dmg` attached to the
  draft release on a tag.
- Linux: WebKitGTK DMABUF renderer disabled by default to fix missing window
  decorations on VMs/software GPUs.
- Local PTY no longer truncates short-lived command output on Linux.

## 0.1.0

Initial release: connection manager (SQLite tree with folder inheritance, vault),
SSH/local/serial sessions with jump hosts and PuTTY .ppk support, xterm.js terminal
with OSC 133 command marks and OSC 7 cwd, SFTP file pane and editor, ssh_config /
PuTTY / MobaXterm import, docs sidebar, and an MCP bridge.
