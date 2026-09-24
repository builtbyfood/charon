# Charon — tester guide (0.2.0 preview)

Charon is one app for the work you'd otherwise spread across PuTTY, MobaXterm and
Notepad++: a connection manager, SSH/serial/local terminals, an SFTP pane that
follows your shell (including as root), an editor with remote save-back, and an
MCP server so AI agents can work in your sessions with your approval.

This is a preview. It works for daily use on the maintainers' machines, but you
are the first people running it on hardware and workflows we haven't seen. The
most useful thing you can do is use it for real work and tell us where it gets
in your way.

---

## 1. Install

Grab the build for your platform from the release page.

### Windows 10/11
- **Portable (recommended for testing):** unzip `charon-windows-portable.zip`
  anywhere and run `charon.exe`. Everything it saves stays in a `data\` folder
  next to the exe — nothing lands in AppData. Delete the folder to uninstall.
- **Installer:** `Charon_0.2.0_x64-setup.exe`. Per-user install, no admin needed.
  Data lives in `%APPDATA%\charon`.
- Needs the WebView2 runtime, which every updated Windows 10/11 already has.

### Linux
- Debian/Ubuntu 22.04+: `sudo apt install ./charon_0.2.0_amd64.deb`
- Fedora / RHEL 9 family: `sudo dnf install ./charon-0.2.0-1.x86_64.rpm`
- Anything else: `chmod +x Charon_0.2.0_amd64.AppImage && ./Charon_0.2.0_amd64.AppImage`
- Portable: unpack `charon-linux-portable.tar.gz` and run `./charon` from that folder.
- Data lives in `~/.config/charon` (installed) or `./data` (portable).

### macOS (unsigned)
- Open the `.dmg`, drag Charon to Applications.
- First launch will say the app is "damaged" or from an unidentified developer.
  That's Gatekeeper reacting to an unsigned build, not corruption. Either
  right-click → Open, or run `xattr -d com.apple.quarantine /Applications/Charon.app`.
- Data lives in `~/Library/Application Support/charon`.

### One rule that will save you an hour
**Do not run Charon elevated ("Run as administrator") while another copy runs
normally.** On Windows, two copies at different privilege levels share a WebView2
profile and the non-elevated one silently stops receiving keyboard input. Portable
mode isolates its profile, installed mode doesn't. If typing ever stops working in
every field at once, this is why.

---

## 2. First run

1. **Portable mode only:** you'll be asked to create a master password for the
   vault (`data/vault.age`). This protects saved passwords and keys. Installed
   mode uses your OS keyring instead and won't ask.
2. **Import your existing sessions.** Sidebar → *Import…*:
   - OpenSSH: reads `~/.ssh/config` (Host, HostName, User, Port, IdentityFile, ProxyJump)
   - PuTTY: reads the registry (Windows) or `~/.putty/sessions` (Linux)
   - MobaXterm: pick your `.mxtsessions` export
   
   You get a preview before anything is written — new vs duplicate, with
   Skip / Overwrite / Import-as-copy. Passwords are never imported; key files
   become vault entries pointing at the file. `.ppk` keys are read directly.
3. **Connect.** Double-click a host, or type `user@host[:port]` in the quick-connect
   box and press Enter.
4. **First connection to any host** shows the host-key fingerprint and asks you to
   accept. Compare it against the server (`ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub`)
   the first time; after that Charon remembers it. If a key ever *changes*, Charon
   refuses to connect and tells you why — that's deliberate.
5. **Password prompt.** If no saved credential resolves, an inline bar appears at
   the bottom of the terminal. Tick *save to vault as…* and it won't ask again.

---

## 3. The connection tree

The left sidebar is a folder tree, and **folders carry settings that hosts inherit**:
user, credential, jump host, colour, tags, startup commands, MCP tier. Set a
jump host on the `prod` folder once and every host inside goes through it. Set a
colour on a folder and every tab from it shows that colour on the tab dot and the
left edge of the terminal — you always know which environment you're typing into.

- Right-click or hover a row for edit/delete. The editor shows inherited values
  greyed out so you can see what a folder is contributing.
- The splitter between sidebar and terminal drags; the width is remembered.
- The filter box matches name, host, and tags.
- *Local* opens a shell on your machine (PowerShell → cmd on Windows, `$SHELL` on
  Linux/Mac). *Local (admin)* opens an elevated one — on Windows 11 24H2+ with
  inline `sudo` it stays in-tab, otherwise it opens a separate elevated window
  and explains why.

---

## 4. The terminal

Things that are different from PuTTY, and worth trying on purpose:

**Command awareness.** On connect, Charon injects a small shell hook (bash, zsh,
fish, PowerShell) that marks where each prompt, command and output begins. It is
sent as one line and never written to the remote's rc files; if the remote already
has shell integration (WezTerm, Kitty, VS Code style) it's skipped. The status bar
chip on the right tells you what it detected: `bash · 133 on · 12 marks`. Click the
chip to re-inject, disable for this session, or copy the hook text. With it on:

- A thin bar in the left gutter marks each command; red if it exited non-zero.
- **Ctrl+↑ / Ctrl+↓** jump between commands (the prompt line flashes where you land).
- **Ctrl+Shift+↑** selects just the output of the current command.
- Right-click → *Copy last output* / *Copy last command*.
- `sudo -i` and `su -` re-inject automatically, so marks continue in the root shell.
- The status bar shows the shell's current directory (OSC 7).

**Copy and paste, cleaned.** Selecting text and copying strips zero-width
characters, non-breaking spaces, BOMs, trailing whitespace, converts smart quotes
to ASCII and CRLF to LF. Pasting runs the same cleaner and warns before sending
multiple lines or hidden characters. Ctrl+Shift+C / Ctrl+Shift+V everywhere;
Ctrl+C sends SIGINT unless text is selected (PuTTY behaviour); middle-click paste
on Linux.

**Right-click, MobaXterm style.** With a selection, right-click copies it. With
none, right-click pastes. Shift+right-click opens the menu. If you'd rather have
the menu on plain right-click, set `terminal.right_click = "menu"` in the profile.

**Flag hints.** Type a command followed by `-` or `--` (e.g. `tar --`) and a popup
lists that command's options with descriptions, sourced from bundled tldr pages
plus the host's own `--help` output (fetched once per host, cached). ↑↓ Enter
Esc; Tab is never intercepted, so remote bash completion keeps working.

**Docs panel.** Sidebar → *Docs* (Ctrl+Shift+D). As you type a command, its tldr
page appears; the *man* button fetches the real man page over the session. Tip of
the day at the top.

**Other:** Ctrl+Shift+F search, Ctrl+= / Ctrl+- / Ctrl+0 zoom (persisted),
rectangular select with Alt-drag, 50 000 lines of scrollback.

---

## 5. Files (SFTP)

The right-hand pane opens with every SSH and local tab (toggle with the *Files*
button or **Ctrl+Shift+E**). It's a normal file browser — sort, hidden files,
upload/download, drag-drop from Explorer/Finder/Nautilus, right-click for open,
rename, delete, chmod, copy path — with two things worth trying:

- **→ cwd**: the pane follows the shell. `cd` in the terminal and the pane moves.
- **→ term**: the reverse. Navigate in the pane and the shell `cd`s to match.
  Both can be on at once.

**Root.** SFTP normally runs as your login user, so after `sudo -i` a plain SFTP
client can't see `/root`. Charon notices, and on the first permission error runs a
second SFTP server through `sudo` on the same connection. You'll be asked for the
sudo password once (or it reuses your saved SSH password if you allow it). The
badge above the listing shows which backend is live: `sftp as admin`,
`sftp as root (sudo)`, or `shell as root` (a fallback for hosts without
`sftp-server`). When you `exit` back to your user, it switches back.

Transfers show progress at the bottom with cancel/retry. Uploading to a path that
already has a transfer in flight asks first. *Preserve mode* copies the source
file's permissions after upload (off by default).

---

## 6. Editor

Double-click a text file in the Files pane, drop a file on the window, or click
*Editor* in the sidebar. It's a Monaco (VS Code engine) editor with Notepad++
habits: multiple documents as inner tabs, encoding and line-ending shown in the
status bar with one-click conversion (LF ⇄ CRLF, UTF-8 / UTF-8-BOM / Latin-1),
*show all characters*, find/replace with regex (Ctrl+F / Ctrl+H), go to line
(Ctrl+G), word wrap, minimap, Format (Shift+Alt+F for JSON/YAML/Markdown),
snippets for shell/python/yaml/ini, F1 command palette.

Remote files save straight back over SFTP — atomically, so a dropped connection
mid-save can't leave a half-written file. **Ctrl+Enter** sends the selected line(s)
to the terminal the file came from. Paste is raw by default here; Ctrl+Shift+V
pastes cleaned.

---

## 7. MCP — letting an agent into your sessions

This is the feature that needs the most careful testing.

Sidebar → *MCP* → Enable. Charon starts a server on `127.0.0.1` with a token that
rotates every launch and is stored read-only to your user. *Copy config* gives you
the JSON for Claude Code, Cursor, or any MCP client.

Every host or folder has an **MCP tier**, inherited like everything else:

| Tier | Agent can |
|------|-----------|
| `read` (default) | list sessions, read scrollback, read connection settings (never credentials) |
| `exec-approve` | everything above, plus run commands and upload files — **each one pops an Approve / Deny dialog in Charon showing the exact command**. 60 s with no answer = deny, and the command is never sent. |
| `trusted` | run without asking |

Commands run hidden (you don't see them scroll past) and only on sessions where
shell integration is active, because that's how Charon knows when they finish and
what they returned. If you're typing in a session, the agent is refused for a few
seconds rather than interleaved with your keystrokes. `data/mcp.log` records tool,
session, tier decision, exit code and duration — never command text or output.

**To test:** set a lab host to `exec-approve`, ask your agent to run `uptime`
there, and try Approve, Deny, and letting it time out. Then check the host's
shell history to confirm only the approved one ran. Please never set production
hosts to `trusted` while testing.

---

## 8. Profile

`data/profiles/default.toml` (portable) or the same path under the config dir. It
covers font, size, renderer, scrollback, copy/paste rules, right-click mode,
keepalive interval, docs options and MCP defaults. Edit it with Charon itself; it
reloads on save. Unknown keys are preserved so a newer build won't eat your edits.

---

## 9. Known limitations in this build

- Serial and RDP/VNC: serial connections exist but have had little real-hardware
  testing; RDP/VNC aren't in yet (planned, in-tab).
- Editor has no language servers yet — completion is word-based plus snippets.
- Two installed (non-portable) copies at different privilege levels on Windows
  break keyboard input in the non-elevated one (see §1).
- macOS builds are unsigned (see §1) and have had the least testing.
- Headless Linux without a Secret Service (no desktop keyring): use portable mode.
- `.ppk` DSA keys are refused (deprecated everywhere; convert to ed25519).

---

## 10. Reporting

Open an issue with:

1. **Platform and mode** — e.g. "Windows 11, portable" or "Fedora 44, rpm". The
   status bar's bottom-left shows `PORTABLE`/`INSTALLED` and the data dir.
2. **What you did, what you expected, what happened.** Screenshots help a lot;
   the terminal, status bar and Files badge all carry state we'll want to see.
3. **Console lines.** Press **Ctrl+Shift+I** → Console. Charon logs its decisions
   with a `[charon]` prefix — shell detection, hook injection, sudo-sftp, MCP.
   Copy the relevant ones.
4. **Remote OS and shell** if it's a session issue (`cat /etc/os-release`, `echo $SHELL`).
5. For MCP issues, the matching lines from `data/mcp.log`.

Things we especially want to hear about: hosts where the shell hook doesn't
detect or breaks something; prompts we don't recognise (custom PS1, oh-my-zsh
themes, Starship); copy/paste that mangled something; SFTP-as-root on distros
other than Ubuntu/Fedora; anything that feels slower than MobaXterm.

Thanks for testing. The goal is a tool you'd actually switch to — tell us what
would stop you.
