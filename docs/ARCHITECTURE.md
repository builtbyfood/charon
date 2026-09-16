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

## Persistence

`AppPaths` decides where everything lives:

- installed: platform config dir (`%APPDATA%\charon`, `~/.config/charon`)
- portable: `./data` next to the exe, triggered by `portable.flag`

Inside: `charon.db` (SQLite: connection tree, credential *references*, command history),
`profiles/*.toml` (appearance, keybinds, docs settings), `vault.age` (secrets, portable
mode only — installed builds use the OS keyring).

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
- [x] SFTP/local file pane (R2-2), Monaco editor tab with remote round-trip (R2-3)

### 3 — MCP server (R2-4, shipped)
- [x] JSON-RPC 2.0 MCP server, HTTP on `127.0.0.1:<port>` with per-launch bearer
  token + a `--mcp-stdio` proxy (implemented directly, not via rmcp, which had
  moved 0.x→3.x and could not be validated here without a client)
- [x] Tools: `list_sessions`, `list_connections`, `get_connection`,
  `read_scrollback`, `run_command`, `sftp_get`, `sftp_put`, `open_in_editor`
- [x] Permission tiers per connection: `read` / `exec-approve` (in-app approval)
  / `trusted`; every call logged to `data/mcp.log`; Settings → MCP UI

### 4 — Docs and tips (R2-5, shipped)
- [x] Bundled tldr-pages (embedded, served over IPC; `tldr_update` refreshes into
  the data dir), remote `man` fetched hidden over the active SSH session
- [x] Sidebar Docs tab shows tldr for the command being typed (OSC 133 input),
  Ctrl+Shift+D
- [x] Tip-of-the-day from `docs/tips.yaml`, community-extendable

### Later
- RDP/VNC (launch external client first, embed later), X11 helper on Windows,
  session recording/replay, team-shared connection trees.
