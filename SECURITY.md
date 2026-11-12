# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue: on this
repository's GitHub page, open **Security → Report a vulnerability** (GitHub's
private vulnerability reporting). Only the maintainers see the report.

Include what you can of:

- the Charon version (status bar, bottom right) and platform (Windows / Linux /
  macOS, installed or portable);
- what an attacker needs (a hostile SSH server, a local user, an MCP client, a
  crafted file…) and what they get;
- steps or a proof of concept.

You should get an answer within a week. Please give us a reasonable time to ship
a fix before publishing details.

## Scope

In scope, for example:

- secrets (saved passwords, key passphrases, the vault master password, the MCP
  token) reaching the UI, logs, disk in clear, or a remote host they weren't
  meant for;
- a remote host or its output making Charon type, send or run something the user
  didn't ask for (hidden-command sentinels, shell-integration markers, sudo
  prompts, OSC sequences);
- the MCP server: anything that gets past the bearer token, the per-session
  permission tier or the approval dialog;
- host-key checking: connecting despite a changed key, or writing to the user's
  `~/.ssh/known_hosts`.

Out of scope: what a host you connect to can already do with your consent (it can
print anything into your terminal), and attacks that need control of your own
user account.

## Supported versions

Only the latest release gets fixes.
