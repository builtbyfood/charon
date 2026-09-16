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
