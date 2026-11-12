// Prompt-line heuristics shared by the terminal (osc133.ts) and tests.

export type Shell = "bash" | "zsh" | "fish" | "pwsh";

/** Guess the remote shell from its first prompt line. Undefined = don't inject. */
export function detectShell(promptLine: string): Shell | undefined {
  const l = promptLine.replace(/\s+$/, "");
  if (/^PS [A-Za-z]:\\|^PS \/|^PS>/.test(l) || /\bPS [^>]*>$/.test(l)) return "pwsh";
  if (/%$/.test(l)) return "zsh";
  if (/[$#]$/.test(l)) return "bash";
  if (/>$/.test(l) && /@/.test(l) && !/^[A-Za-z]:\\/.test(l)) return "fish";
  return undefined;
}

/**
 * Stricter than detectShell, for re-injection into nested shells while a command
 * is "running": only prompts that carry a user@host, a bracketed [user@host dir],
 * or are a bare `#`/`$`. Keeps `postgres=#`, `mysql>`, `>>>` from being fed a hook.
 */
export function looksLikeShellPrompt(line: string): Shell | undefined {
  const l = line.replace(/\s+$/, "");
  if (!(/@/.test(l) || /\][#$]$/.test(l) || /^[#$]$/.test(l))) return undefined;
  return detectShell(l);
}

/** OSC 7 payload `file://host/path` (optionally %XX-encoded) → `/path`. */
export function parseOsc7(data: string): string | undefined {
  const m = /^file:\/\/[^/]*(\/.*)$/.exec(data.trim());
  if (!m) return undefined;
  try { return decodeURIComponent(m[1]); } catch { return m[1]; }
}

/** The user in a shell prompt (`admin@host:~$`, `[root@host ~]#`), or undefined. */
export function promptUserOf(line: string): string | undefined {
  return /(?:^|\s|\[)([A-Za-z_][\w.-]*)@[\w.-]+/.exec(line)?.[1];
}

/** A line waiting on a sudo/su password: `[sudo] password for X:` or a bare `Password:`. */
export function looksLikeSudoPrompt(line: string): boolean {
  const l = line.replace(/\s+$/, "");
  if (!l.endsWith(":")) return false;
  return /\[sudo\] password for .+:$/.test(l) || /(?:^|\s)[Pp]assword:$/.test(l);
}

/** The user named in a `[sudo] password for <user>:` prompt, or null. */
export function sudoUserOf(line: string): string | null {
  return /\[sudo\] password for ([^:]+):/.exec(line)?.[1]?.trim() ?? null;
}

/**
 * Whether a prompt is eligible for *automatic* password send: only the login
 * user's own `[sudo] password for <login-user>:`. A prompt for someone else, or a
 * bare `Password:` (su), is never auto-sent — it stays a manual button.
 */
export function sudoAutoEligible(line: string, loginUser?: string): boolean {
  return !!loginUser && sudoUserOf(line) === loginUser;
}

/**
 * May the saved password go to a sudo prompt without a click? A remote can print
 * `[sudo] password for <you>:` whenever it likes, so the text alone is not enough:
 * - it must be the login user's own prompt (`eligible`) and auto-send be on;
 * - shell integration must be on and a command be running (OSC 133 C seen, no D);
 * - that command must be one the user started: an Enter typed (or broadcast) in
 *   this tab since the last prompt `armed` it, and a send spends it, so there is
 *   at most one automatic send per command. A new prompt disarms it; nothing the
 *   remote prints can arm it.
 * Anything else still shows the button (one click sends).
 */
export function sudoAutoAllowed(s: { pref: boolean; hasPassword: boolean; eligible: boolean; integration: boolean; commandOpen: boolean; armed: boolean }): boolean {
  return s.pref && s.hasPassword && s.eligible && s.integration && s.commandOpen && s.armed;
}
