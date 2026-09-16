# Shell integration (OSC 133)

Charon injects one of these on connect so the terminal knows where every prompt,
command and output begins and ends. Same protocol as WezTerm, Kitty, iTerm2 and
Windows Terminal, so the hooks are harmless if the user already has them.

Markers (BEL-terminated: `ESC \` inside bash's `\[ \]` prompt escapes leaks a
stray `]`, and BEL is accepted by every terminal; `\033` not `\e` for dash/sh):

- `ESC ] 133 ; A BEL` — prompt start
- `ESC ] 133 ; B BEL` — command input start (user is typing)
- `ESC ] 133 ; C BEL` — command output start (Enter pressed)
- `ESC ] 133 ; D ; <exit> BEL` — command finished with exit code
- `ESC ] 7 ; file://<host><path> BEL` — current directory (after D), so the SFTP
  pane can follow the shell. Not percent-encoded; the terminal decodes `%XX` if present.

Injection is per-connection (`settings.shell_integration`, default on). Charon
detects the shell from the first prompt it sees (`$`/`#` bash, `%` zsh, `user@host …>`
fish, `PS …>` PowerShell) and types the hook in — `eval "$(cat <<'CHARON_EOF' … )"`
for bash/zsh, line by line for fish and PowerShell — so nothing is written to the
remote user's rc files. The block is typed with a leading space (out of history)
and ends with `printf '\033]7770;end\a'`: the terminal swallows everything from the
moment it starts typing until that private OSC arrives, so the user only sees the
prompt come back. If the remote already emits OSC 133, nothing is injected.
The hooks are also imported with Vite's `?raw` by `src/lib/osc133.ts`, so keep
them free of anything that only works when sourced from a file.
