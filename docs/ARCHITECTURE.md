# Architecture and phase plan

## Data flow

```
 Svelte UI ──invoke()──▶ Tauri commands (Rust) ──▶ SessionManager
     ▲                                                 │
     └──── listen("term:data:{id}") ◀── events ◀───────┘
                                              ├─ SshSession   (russh)
                                              ├─ LocalPty     (portable-pty)
                                              └─ SerialSession(serialport)
```

Every session is an `id` + a byte channel. The frontend never cares what's behind it.
Output goes to the UI as `term:data:{id}` events carrying raw bytes (base64 in JSON),
input comes back through `session_write(id, data)`.

## Windows and moving tabs

Every window runs `App.svelte`. `main` has the sidebar, the editor tab, MCP
approvals and vault unlock; tab windows (`tab-<token>`) hold terminal tabs only.
A detached window's file pane opens files in the main window's editor
(`editor:open` event). Closing the main window ends the app; closing a tab window
closes its tabs' sessions. Saving the profile in one window emits `profile:saved`
so the others reload theirs.

A terminal tab moves between windows without reconnecting, because the session
lives in the backend (`tabmove.rs`):

1. The source tab calls `session_hold`. Output for that session is buffered in
   the backend instead of emitted, and an empty data event marks the cut. Every
   byte before the cut is already in the source terminal.
2. The source serialises its screen (xterm serialize addon, at its current size)
   with command marks, line timestamps, cwd and shell state, and stashes it
   (`tab_stash`, a one-time token).
3. `tab_drop_target` reads the cursor and finds the Charon window under it. The
   token goes to that window (`tab_send` → `tab:adopt`), or to a new window
   (`tab_window_open`, which reads the token from its label).
4. The target takes the stash, writes the screen back at the same size, restores
   marks and stamps, starts listening, then `session_release`. That emits what
   was held (and a close that happened meanwhile). Then it fits its own size; the
   resize reaches the remote, so full-screen programs redraw.

The source tab's `onDestroy` skips `session_close` once it has handed off. If the
move fails, `undetach` releases the hold and the tab stays. Dragging uses pointer
events, not HTML5 drag-and-drop: the native file-drop handler turns that off in
the webview on Windows.

## Talking to a remote host

One SSH connection carries several channels; which one a feature uses matters.

- **Interactive shell** (the terminal). *Hidden commands* (`hidden.rs`,
  `Session::exec_hidden`) are typed into it, wrapped in OSC 7771 sentinels, and their
  echo and output are swallowed. They see the shell as the user has it, including
  after `sudo -i`, so the root shell backend (`ShellFs`, Files pane and `#`
  documents) and MCP `run_command` use them. Because they are keystrokes, the SSH
  task only types one when `PromptState` (OSC 133 marks plus the user's own input)
  says the shell sits at a prompt with an empty line. With vim or nano in the
  foreground, or a half-typed command, the call fails with the reason instead. A
  multi-line command (a heredoc) is wrapped in `{ …
}` so the end sentinel never
  shares a line with the heredoc terminator. Without shell integration there are
  no marks and the check cannot apply.
- **Exec channels** (`Session::exec_channel`): a separate non-interactive command
  per call, no pty, stdout only, never touches the terminal. Anything that can run
  while the user types uses this: the flag picker's `--help` (`help_fetch`), the
  docs pane's `man` (`man_fetch`), and the sftp-server probe before sudo-sftp.
- **Shell choice**: a connection's `remote_shell` (or the tab's shell menu) starts
  that shell with an exec request on the pty channel instead of a shell request.
  Unix shells by name run as login shells (`zsh -l`); others run as written
  (`ssh::shell_command`). The menu lists `/etc/shells`, or `where pwsh powershell
  cmd` on Windows, over an exec channel (`session_shells`).
- **SFTP subsystem** (`Session::fs`, `SftpFs`): listings, editor saves, transfers.
  Each request gets `SFTP_REQUEST_TIMEOUT_SECS` (120 s; russh-sftp defaults to
  10 s). Uploads end with shutdown (drain write acks, close), not flush, which
  would fsync the whole file on the server.
- **sudo-sftp** (`Session::sudo_fs`): `sudo -S -p '' sftp-server` on its own
  channel, for Edit/Open as root; one per session, shared by pane and editor.

Saves on every backend are atomic (temp file, rename) and restore the original's
owner and mode afterwards: chown first, then chmod, since chown clears setuid.

## Persistence

`AppPaths` decides where everything lives:

- installed: platform config dir (`%APPDATA%\charon`, `~/.config/charon`)
- portable: `./data` next to the exe, triggered by `portable.flag`

Inside: `charon.db` (SQLite: connection tree, credential *references*, command history),
`profiles/*.toml` (appearance, keybinds, docs settings), `vault.age` (secrets, portable
mode only — installed builds use the OS keyring).

Renaming a credential (`vault_rename`) writes the new vault entry before deleting
the old one. Then every node whose own `credential` named it is updated, so
inheriting hosts follow through their folder.

## Phases

### 1 — Terminal + connections (Round 1, shipped)
- [x] Tauri 2 project, portable-mode detection, CI matrix
- [x] Connection tree model with folder inheritance (`connections.rs`)
- [x] SSH session over russh with password/key auth (`ssh.rs`), jump hosts, host keys
- [x] xterm.js tab, sidebar tree, quick-connect bar, host/folder editor
- [x] Local PTY (incl. elevated via Windows sudo / `sudo -i`), serial (untested on hardware)
- [x] Credential vault (keyring/age), PuTTY `.ppk` keys
- [x] Importers: PuTTY registry, `~/.ssh/config`, `.mxtsessions` (preview + duplicate policy)
- [ ] RDM export importer

### 2 — Command awareness, editor, clean clipboard
- [x] OSC 133 parsing in `osc133.ts`: per-command marks, exit-code gutter, jump/select/copy
- [x] Shell hooks in `shell-integration/` injected on connect (opt-out per connection)
- [x] Sanitised copy (`clipboard.ts`): strip ZW chars, NBSP, BOM, trailing spaces, CRLF→LF,
  smart quotes (toggle); paste guard with bracketed paste; MobaXterm-style right-click
- [ ] Rectangular select
- [x] Links: a link provider over `links.ts` (plain URLs, also across soft wraps and
  TUI hard breaks at the right edge) plus xterm's OSC 8 `linkHandler`; Ctrl/Cmd+click
  opens through the shell plugin. OSC 52 writes reach the clipboard (reads ignored).
  When a program has mouse tracking on, Shift/Option+drag selects and right/middle
  clicks stay with Charon.
- [x] Line timestamps (`stamps.ts`): one xterm marker per second in which a line
  started, always recorded (hidden tabs stamp buffered output with its arrival
  time). The gutter is a status-bar switch saved as `terminal.timestamps`.
- [x] Tabs: reorder by dragging, tear off into a window, drop on another window
  (see "Windows and moving tabs").
- [x] SFTP/local file pane (R2-2), Monaco editor tab with remote round-trip (R2-3)

### 3 — MCP server (R2-4, shipped)
- [x] JSON-RPC 2.0 MCP server, HTTP on `127.0.0.1:<port>` with per-launch bearer
  token + a `--mcp-stdio` proxy (implemented directly, not via rmcp, which had
  moved 0.x→3.x and could not be validated here without a client)
- [x] Tools: `list_sessions`, `list_connections`, `get_connection`,
  `read_scrollback`, `run_command`, `sftp_get`, `sftp_put`, `open_in_editor`
- [x] Permission tiers per connection: `read` / `exec-approve` (in-app approval)
  / `trusted`; Settings → MCP UI
- [x] Every call, denied ones too, is one JSON line in `data/mcp.log`: `ts`, `tool`,
  `session`, `tier`, `decision`, `ok`, `exit_code`, `duration_ms`, plus `subject`
  (the command line for `run_command`, the paths for transfers and editor opens)
  and `error` when it failed (both capped at 2000 chars). The call log in
  Settings → MCP shows both. Commands are logged verbatim, so a secret typed into
  one ends up in the log.

### 4 — Docs and tips (R2-5, shipped)
- [x] Bundled tldr-pages (embedded, served over IPC; `tldr_update` refreshes into
  the data dir), remote `man` fetched over its own exec channel on the session
- [x] Sidebar Docs tab shows tldr for the command being typed (OSC 133 input),
  Ctrl+Shift+D
- [x] Tip-of-the-day from `docs/tips.yaml`, community-extendable

### Later
- RDP/VNC (launch external client first, embed later), X11 helper on Windows,
  session recording/replay, team-shared connection trees.
