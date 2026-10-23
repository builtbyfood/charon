# Changelog

All notable changes to Charon are recorded here. Versions follow semver; the
canonical version lives in `src-tauri/tauri.conf.json` and is propagated to
`package.json` and `Cargo.toml` by `npm run bump` (`scripts/bump-version.mjs`).

## 0.2.6

### Tabs and windows
- **Drag tabs**: drag a tab along the bar to reorder it. Drop it outside the
  window and it opens in its own window, still connected, with its scrollback,
  command marks, file pane and cwd. Drop a tab on another Charon window and it
  joins that window's tabs at the drop point. A tab window closes when its last
  tab leaves.
- Right-click a tab: *Move to new window*, *Move to main window*, *Close tab*.
- Tab windows have no sidebar. Files opened from their file pane go to the
  editor in the main window. Closing the main window quits Charon.

### Connections
- **Rename a credential**: the ✎ button or a double-click in the Credentials
  list. Every connection and folder that used the old name follows.
- **Choose the remote shell**: a new *Shell* field on SSH hosts and folders
  (inherited; blank = the account's login shell). The tab's shell chip menu lists
  the shells the host has (`/etc/shells`, or pwsh / powershell / cmd on Windows
  servers) and reconnects into the one you pick, saving it on the connection.

### Terminal
- **Timestamps**: a *time on/off* switch in the status bar (and the right-click
  menu) shows when each line was printed. Times are always recorded, so switching
  it on also covers earlier output. The setting applies to all tabs and is saved as
  `terminal.timestamps`.

## 0.2.5

### Terminal
- **Clickable URLs**: Ctrl+click (Cmd+click on macOS) opens a URL in the browser;
  hovering shows it. URLs broken over several rows are one link — soft-wrapped, or
  hard-broken at the right edge and indented the way Claude Code and other TUIs lay
  out text. OSC 8 hyperlinks work too (http/https only).
- **Copying from Claude Code and other mouse-using programs**: such programs take
  the mouse, so a plain drag selected nothing and every copy route had nothing to
  copy. Now:
  - OSC 52 is supported: when the program copies (Claude Code does on its own
    selection), the text goes to the system clipboard. Requests to *read* the
    clipboard are ignored.
  - Shift+drag (Option+drag on macOS) selects as usual; the first plain click in
    such a program shows a hint saying so. The Copy button, Ctrl+C,
    Ctrl+Shift+C and right-click then work on that selection.
  - Right-click and middle-click stay Charon's (menu / copy / paste) instead of
    being sent to the program.
- Tips: Shift+drag, Ctrl+click a URL.

## 0.2.4

### Fixes
- Flag picker: `--help` is fetched on its own exec channel instead of the terminal's
  shell. The hidden command started with Ctrl+U, which wiped the half-typed line —
  typing `df -h` ran only `h`.
- Large SFTP transfers no longer time out: requests get 120 s instead of
  russh-sftp's 10 s, and uploads no longer fsync on the server before closing.
  A write the server refuses still fails the upload.
- **Text appearing in files edited in the terminal.** Charon's hidden shell commands
  (Files pane over the root shell, saving `#` documents, MCP `run_command`) were typed
  into the terminal whatever was running — with vim or nano open they went into the
  file. They now run only at an idle prompt with an empty command line (OSC 133) and
  otherwise fail with the reason; the docs pane's `man` lookup has its own channel.
- Saving through the root shell: the heredoc's end marker shared a line with the
  completion sentinel, so it never matched and the heredoc swallowed the next input.

### MCP
- The call log records **what each call asked for**: `subject` holds the command
  line for `run_command` and the paths for `sftp_get`, `sftp_put` and
  `open_in_editor`; `error` holds why a call failed. Both show in Settings → MCP.
  Commands are logged verbatim, so a secret typed into one ends up in
  `data/mcp.log`.

### Docs
- `docs/ARCHITECTURE.md`: new *Talking to a remote host* section (interactive
  shell vs exec channels vs SFTP vs sudo-sftp, and which features use which), plus
  the MCP log format.

## 0.2.3

### Terminal
- Selecting a tab puts the cursor in its terminal (or its password field while
  connecting) — no extra click before typing.

## 0.2.2

### Files / editor
- **Edit as root, on demand** — the Files pane badge offers *browse as root (sudo)*
  at any time (not only after EACCES while the shell is root); right-click a file →
  *Edit as root*, a directory → *Open as root*. One sudo-sftp channel per session is
  shared by the pane and editor and asks for the sudo password once (or uses the
  stored credential).
- Opening the sudo channel no longer needs an idle shell: the sftp-server probe runs
  on its own exec channel, so *Retry as sudo* works while the terminal is in vim.
- A save that fails with *permission denied* offers **Retry as sudo**; documents bound
  to sudo carry a red **root** badge.
- Saves keep the original **owner and mode** (stat first; chown, then chmod, after the
  rename — setuid/setgid survive), over SFTP, sudo-sftp and the shell backend.
- MCP `open_in_editor` takes `as_root`: gated like `run_command` plus the sudo opt-in.
  Agent opens no longer fall back to the root shell on EACCES by themselves.

### Diagnostics
- `[charon] cwd: osc7 …` and `[charon] files: follow/push … go|<reason>` log lines for
  the pane's → cwd / → term toggles.

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
