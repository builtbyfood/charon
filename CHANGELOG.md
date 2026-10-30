# Changelog

All notable changes to Charon are recorded here. Versions follow semver; the
canonical version lives in `src-tauri/tauri.conf.json` and is propagated to
`package.json` and `Cargo.toml` by `npm run bump` (`scripts/bump-version.mjs`).

## 0.3.1

### Tabs
- **Tabs reopen at the next start.** The terminal tabs open when Charon closes
  come back, including tabs in torn-off windows (they reopen in the main
  window). The tab that was visible in the main window is visible again.
  - Saved connections are looked up again, so renames and colour or font
    changes since then apply, and deleted ones are skipped. Quick-connect,
    local and serial tabs reopen as they were.
  - Elevated local shells (which would prompt for UAC / sudo at launch) and
    editor files are not reopened.
  - Closing a torn-off window on its own closes its tabs for good; quitting
    keeps them all.
  - With a locked portable vault, tabs reopen after the unlock prompt.
  - On by default; Settings → Tabs → "Reopen tabs from last time"
    (`[ui] restore_tabs` in the profile). The list lives in
    `data/open-tabs.json` and is kept current while Charon runs, so a crash
    doesn't lose it either.

## 0.3.0

### Appearance
- **Light and dark themes**, set in Settings → Appearance: Dark (the default),
  Light, or Match system, which follows the OS live.
  - Each theme has its own UI colours, a tuned 16-colour terminal palette and
    a matching editor theme.
  - Tests check text contrast against WCAG for every surface and every ANSI
    colour.
  - The app starts in the last theme it used, so there's no dark flash.
- **Native controls follow the theme.** Dropdowns, scrollbars and pickers use
  the theme's colour scheme, which fixes the hard-to-read `<select>` lists
  (e.g. the credential editor) under GTK / Fedora.

### Terminal
- **Per-connection terminal font and size**, as in PuTTY saved sessions: Edit
  → Terminal font. Set on a folder, everything under it inherits; blank uses
  the profile's font.
  - A connection with its own size zooms with Ctrl+= / Ctrl+- on its own tabs
    only, and the zoom isn't saved.
  - The default font and size are in Settings → Appearance.
- The font field lists the installed monospace fonts: fontconfig on Linux, and
  a probed list of common terminal fonts everywhere. Any name can still be
  typed. A live preview shows the result.

### Import
- **The PuTTY import brings each session's font over**, family and size
  (points become px): `Font` / `FontHeight` from the Windows registry, or the
  `client:` font description from Unix PuTTY.
  - Sessions still on PuTTY's factory default (Courier New 10) are left on
    Charon's default font.
  - X11 server fonts (`server:fixed`) can't be used, so they're reported in
    the import warnings.
  - Overwrite on re-import also updates the font.

## 0.2.9

### Files
- **The file pane works like a file manager** on the same filesystem (one SSH
  host, sudo-sftp, the shell backend or the local disk):
  - Drag rows onto a folder, a breadcrumb or ↑ to move them; hold Ctrl to copy.
    Esc cancels the drag.
  - New right-click items: Cut, Copy, Paste / Paste into folder, Duplicate,
    Move to… and Copy to… (type a folder path), and New file….
  - Keys: Ctrl+C / Ctrl+X / Ctrl+V, Delete, F2 to rename, Backspace or Alt+↑ to
    go up.
  - Copies run as transfers, with progress, cancel and retry of failed files.
  - You're asked about name clashes only when the destination already has one
    (Overwrite / Skip / Keep both). Copying into the same folder always keeps
    both, as "name (2)".
- **Delete removes whole folders**, contents included. It used to need them
  empty. The confirmation says so.

### Credentials
- **Edit a credential**, not just its name. ✎ (or a double-click) opens a form
  for the name, the kind (password / private key), the password or key path,
  and the key passphrase, with an option to remove the passphrase.
  - Stored values are still never shown. A blank field keeps what is stored.
  - Changing the kind needs a new value.
  - Connections follow a rename, as before.

## 0.2.8

### Terminal
- **Settings → Terminal → Renderer**: WebGL (default), Canvas or DOM, switched
  live for every tab (new *Settings* button in the sidebar). `appearance.renderer`
  in the profile now also accepts `"canvas"`.
- Terminal output reaches its tab over a per-session channel instead of a global
  event; a tab moving windows takes the channel over. On Linux the GTK main loop
  is woken after every message to the UI (terminal output, file transfer
  progress, MCP prompts, host-key prompts).

### Diagnostics
- `[charon]` console lines are copied to stdout, so a release build run from a
  terminal shows them without devtools.
- **Trace output timing** (Settings, or `CHARON_TRACE_OUTPUT=1`): every output
  chunk logs `out: emit` (Rust), `out: recv`, `out: parsed` and `out: painted`
  (JS) with one sequence number and its lag since the emit.
- The Linux frame fallback says once if frames arrive on their own
  (`raf: … fallback=0`), as well as whenever the timer stepped in.

## 0.2.7

### Linux
- **Output no longer waits for a keypress** (Fedora / GNOME Wayland): WebKitGTK
  could stop running frame callbacks until the next key or click, so `echo hello`
  or a running `ping` showed up one update late. Every frame request now also
  arms a 50 ms timer that renders if the frame never comes (terminal, file pane
  and editor alike). The dev console logs `[charon] raf: native=… fallback=…`
  when the timer had to step in.
- **Copy and paste work on Wayland**: the terminal, file pane and editor read and
  write the clipboard through GTK instead of the clipboard plugin, whose Wayland
  path needs a protocol GNOME doesn't offer. Failures now log
  `[charon] clipboard: … failed`.

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
