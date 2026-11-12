//! Hidden commands on an interactive shell. The line is typed into the session
//! wrapped in private OSC sentinels; the session's output task feeds every byte
//! through `Capture`, which swallows the echo, collects the payload between the
//! sentinels, then keeps swallowing the redrawn prompt (up to the next `133;B`
//! mark or a short silence) so the user's screen does not change at all.
//!
//! Pure state machine, tested; the SSH task and the local PTY's emitter thread
//! drive it. POSIX shells get `wrap`, PowerShell gets `wrap_pwsh`; both print the
//! same sentinels, so one `Capture` serves every shell.
//!
//! Every sentinel carries a fresh random `nonce` per command, so output the
//! command prints (a hostile file being `cat`ed, a listing) can't fake the end
//! of the payload or inject `7771;o` chunks: it would have to guess the nonce.

use base64::Engine;
use std::time::{Duration, Instant};

/// A fresh nonce for one hidden command: 16 hex digits from the OS RNG.
pub fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}
/// `ESC ] 7771 ; s ; <nonce> BEL` — payload starts after this.
fn start(nonce: &str) -> Vec<u8> {
    format!("\x1b]7771;s;{nonce}\x07").into_bytes()
}
/// `ESC ] 7771 ; e ; <nonce> ; <exit code> BEL` — payload ends before this.
fn end_prefix(nonce: &str) -> Vec<u8> {
    format!("\x1b]7771;e;{nonce};").into_bytes()
}
/// `ESC ] 7771 ; o ; <nonce> ; <base64> BEL` — a piece of the output, sent inside
/// the sentinel stream (PowerShell). When a payload has any, they are the output.
fn out_prefix(nonce: &str) -> Vec<u8> {
    format!("\x1b]7771;o;{nonce};").into_bytes()
}
/// `ESC ] 133 ; B` (BEL or ESC \ terminated) — prompt redrawn, screen state restored.
const PROMPT_END: &[u8] = b"\x1b]133;B";
/// No hook in this shell: after the end sentinel, this much silence ends the tail swallow.
pub const TAIL_QUIET: Duration = Duration::from_millis(300);
/// Give up on a command that never produces its end sentinel.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// MCP `run_command` and root. A command that starts with `sudo` is not typed into
/// the terminal: Charon runs the rest as root on its own channel (no terminal,
/// the password on sudo's stdin only; see `root_exec`). `Ok(Some(rest))` = run
/// `rest` as root; `Ok(None)` = an ordinary command; `Err` = refused: sudo
/// options (`sudo -u x`), or `sudo` / `su` anywhere else in the command, which
/// would wait for a password nobody answers.
pub fn root_command(cmd: &str) -> Result<Option<String>, String> {
    let words: Vec<&str> = cmd
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ';' | '&' | '|' | '(' | ')' | '`' | '{' | '}' | '\'' | '"'
                )
        })
        .filter(|w| !w.is_empty())
        .collect();
    let is_root_word = |w: &&str| matches!(*w, "sudo" | "su" | "doas" | "pkexec");
    let trimmed = cmd.trim_start();
    if trimmed.split_whitespace().next() == Some("sudo") {
        let rest = trimmed["sudo".len()..].trim();
        if rest.is_empty() {
            return Err("sudo needs a command".into());
        }
        if rest.starts_with('-') {
            return Err(
                "sudo options are not supported: Charon runs the command as root with plain sudo"
                    .into(),
            );
        }
        if words.iter().skip(1).any(is_root_word) {
            return Err(
                "only one sudo, at the start: the rest of the command already runs as root".into(),
            );
        }
        return Ok(Some(rest.to_string()));
    }
    if words.iter().any(is_root_word) {
        return Err("put sudo at the start of the command (Charon then runs it as root on its own channel); su, doas and pkexec are not supported".into());
    }
    Ok(None)
}

/// The command line that runs `script` as root through sudo, outside any
/// terminal. `-k` ignores cached credentials and `-p ''` prints no prompt, so with
/// `with_password` sudo reads exactly one line (the password) from stdin before
/// it starts anything; without it (`sudo -n` succeeded: NOPASSWD), `-n` makes
/// sure it never asks. `exec </dev/null` runs before the script, so the script
/// can't read stdin either way.
pub fn root_exec(script: &str, with_password: bool) -> String {
    let inner = sq(&format!("exec </dev/null; {script}"));
    if with_password {
        format!("sudo -S -k -p '' -- sh -c {inner}")
    } else {
        format!("sudo -n -k -- sh -c {inner}")
    }
}

/// Does sudo run without a password here (NOPASSWD)? Exit 0 = yes.
pub const ROOT_PROBE: &str = "sudo -n -k -- true";

/// Shell-quote for POSIX sh.
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Remove ANSI escape sequences (OSC and CSI) from captured output. The shell's
/// OSC 133 hook fires its preexec `133;C` marker before every simple command in
/// the wrapped line, so those land *inside* the capture window; strip them (and
/// any colour/CSI a command emits) before parsing `ls`/`stat`.
pub fn strip_escapes(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != 0x1b {
            out.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b']') => {
                // OSC: to BEL or ESC \.
                i += 2;
                while i < b.len() {
                    if b[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if b[i] == 0x1b && b.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            Some(b'[') => {
                // CSI: to a final byte 0x40..=0x7e.
                i += 2;
                while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                    i += 1;
                }
                if i < b.len() {
                    i += 1;
                }
            }
            Some(_) => i += 2, // two-byte escape
            None => i += 1,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The keystrokes for one hidden command. Ctrl+U first clears the prompt line
/// (normally empty: `PromptState` refuses a half-typed one); the leading space keeps
/// it out of history. A multi-line command (a heredoc) goes inside `{ …\n}` so the
/// end sentinel sits on a line of its own after the heredoc terminator — appended to
/// the terminator line it would stop it matching, and the heredoc would swallow
/// whatever is typed next. `nonce` is hex, so it needs no quoting.
pub fn wrap(cmd: &str, nonce: &str) -> String {
    let end = format!("printf '\\033]7771;e;{nonce};%s\\a' \"$?\"");
    if cmd.contains('\n') {
        format!("\x15 printf '\\033]7771;s;{nonce}\\a'; {{ {cmd}\n}}; {end}\n")
    } else {
        format!("\x15 printf '\\033]7771;s;{nonce}\\a'; {cmd}; {end}\n")
    }
}

/// The syntax a hidden command is wrapped in, from the shell running in the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    Pwsh,
}

impl Dialect {
    /// From a shell name (as the UI detects it) or a program path. fish has no `$?`
    /// and cmd.exe no way to print the sentinels, so they (and anything unknown) get none.
    pub fn of(shell: &str) -> Option<Dialect> {
        let base = shell
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(shell)
            .to_ascii_lowercase();
        match base.strip_suffix(".exe").unwrap_or(&base) {
            "bash" | "zsh" | "sh" | "dash" | "ksh" | "mksh" | "ash" => Some(Dialect::Posix),
            "pwsh" | "pwsh-preview" | "powershell" => Some(Dialect::Pwsh),
            _ => None,
        }
    }

    pub fn wrap(self, cmd: &str, nonce: &str) -> String {
        match self {
            Dialect::Posix => wrap(cmd, nonce),
            Dialect::Pwsh => wrap_pwsh(cmd, nonce),
        }
    }
}

/// The keystrokes for one hidden command in PowerShell (5.1 and 7). Like the hook
/// injection, the command travels as one base64 line through Invoke-Expression:
/// PSReadLine would turn a multi-line command into `>>` continuation, and base64
/// sidesteps quoting. A `try` keeps a terminating error from skipping the end
/// sentinel. No Ctrl+U (PSReadLine's Windows mode doesn't bind it); `PromptState`
/// already refuses a non-empty line. CR is the Enter key.
///
/// Output travels in the sentinel stream, not on the screen: collected with
/// `*>&1 | Out-String` and written as base64 `7771;o` chunks (`OUT_PREFIX`). On
/// Windows, ConPTY passes OSC sequences straight through but repaints text
/// later, so printed output (`Write-Output x`, even through `Out-Host` or
/// `[Console]::Write`) arrived after the end sentinel and was swallowed with the
/// tail. A slow native command (`cmd /c ...`) hid this by giving the repaint time.
///
/// Exit code: the hook's `133;D` mapping (`$?` → 0, else `$LASTEXITCODE`, else 1),
/// evaluated right after the command. The `133;D` the prompt prints afterwards
/// would see the sentinel write's own `$?`. Two adjustments make it independent of
/// whether Invoke-Expression passes a failure on in `$?`: `$LASTEXITCODE` is
/// cleared first, so a nonzero value is this command's native exit code (it wins,
/// stale codes can't leak in); and a new entry at the head of `$Error` counts as
/// a failure.
pub fn wrap_pwsh(cmd: &str, nonce: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
    let esc = "$([char]27)";
    let bel = "$([char]7)";
    format!(
        " [Console]::Write(\"{esc}]7771;s;{nonce}{bel}\"); $global:LASTEXITCODE = 0; $__charon_e = $Error[0]; \
$__charon_o = ''; \
try {{ $__charon_o = Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b64}'))) *>&1 | Out-String; $__charon_ok = $? }} \
catch {{ $__charon_ok = $false; $__charon_o += $_ | Out-String }}; \
$__charon_b = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$__charon_o)); \
for ($__charon_i = 0; $__charon_i -lt $__charon_b.Length; $__charon_i += {OUT_CHUNK}) {{ \
[Console]::Write(\"{esc}]7771;o;{nonce};\" + $__charon_b.Substring($__charon_i, [Math]::Min({OUT_CHUNK}, $__charon_b.Length - $__charon_i)) + \"{bel}\") }}; \
if ($Error.Count -and -not [object]::ReferenceEquals($Error[0], $__charon_e)) {{ $__charon_ok = $false }}; \
[Console]::Write(\"{esc}]7771;e;{nonce};$(if ($global:LASTEXITCODE) {{ $global:LASTEXITCODE }} elseif ($__charon_ok) {{ 0 }} else {{ 1 }}){bel}\")\r"
    )
}

/// Windows: a command that would raise a UAC prompt, which shows on the secure
/// desktop where nobody answers for an agent, so the hidden command would hang
/// until it timed out. Returns what gave it away. Errs on the side of refusing
/// (`echo sudo` is refused too). A program that merely *needs* elevation fails on
/// its own (ERROR_ELEVATION_REQUIRED) and isn't caught here; only
/// ShellExecute-style elevation prompts.
pub fn uac_trigger(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_ascii_lowercase();
    let words = lower.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
    for w in words {
        match w {
            "sudo" | "sudo.exe" => return Some("sudo"),
            "gsudo" | "gsudo.exe" => return Some("gsudo"),
            // runas.exe, and Start-Process -Verb RunAs (also -Verb:RunAs, via the split on ':').
            "runas" | "runas.exe" => return Some("RunAs"),
            _ => {}
        }
    }
    None
}

/// Whether the interactive shell can take a hidden command right now, from the
/// OSC 133 marks in its output and the user's own keystrokes. A hidden command is
/// typed into the terminal, so it is only safe at a prompt with an empty line: with
/// vim or nano in the foreground it would be typed into the file being edited, and
/// a half-typed command line would be erased by the leading Ctrl+U.
#[derive(Default)]
pub struct PromptState {
    /// Any 133 mark seen. Without shell integration there is nothing to go on, so
    /// hidden commands are allowed as before.
    seen: bool,
    at_prompt: bool,
    /// The user typed since the prompt was drawn.
    typed: bool,
    /// Tail of the previous chunk, for a mark split across chunks.
    tail: Vec<u8>,
    /// Cursor-position queries the shell is still waiting on (PSReadLine on Unix
    /// asks right after drawing the prompt). Typed meanwhile, a hidden command is
    /// read as the answer and lost. Plus the tail for a query split across chunks.
    queries: usize,
    query_tail: Vec<u8>,
}

impl PromptState {
    const MARK: &'static [u8] = b"\x1b]133;";

    /// Session output (everything the shell prints, captured or not).
    pub fn output(&mut self, data: &[u8]) {
        let mut q = std::mem::take(&mut self.query_tail);
        q.extend_from_slice(data);
        self.queries += q.windows(DSR.len()).filter(|w| *w == DSR).count();
        self.query_tail = q[q.len() - (DSR.len() - 1).min(q.len())..].to_vec();
        self.marks(data);
    }

    /// A cursor query was answered (by the terminal, or by `Capture` for one it swallowed).
    pub fn answered(&mut self, n: usize) {
        self.queries = self.queries.saturating_sub(n);
    }

    /// The shell is waiting for the terminal to answer a cursor query.
    pub fn awaiting_report(&self) -> bool {
        self.queries > 0
    }

    fn marks(&mut self, data: &[u8]) {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(data);
        let mut i = 0;
        while let Some(at) = find(&buf[i..], Self::MARK) {
            let k = i + at + Self::MARK.len();
            let Some(&kind) = buf.get(k) else {
                // Mark cut off right before its letter: keep it for the next chunk.
                self.tail = buf[i + at..].to_vec();
                return;
            };
            match kind {
                b'A' | b'B' => {
                    self.seen = true;
                    self.at_prompt = true;
                    self.typed = false;
                }
                b'C' | b'D' => {
                    self.seen = true;
                    self.at_prompt = false;
                }
                _ => {}
            }
            i = k + 1;
        }
        // Keep enough to finish a mark cut off inside its prefix.
        let keep = (Self::MARK.len() - 1).min(buf.len() - i);
        self.tail = buf[buf.len() - keep..].to_vec();
    }

    /// The user's keystrokes or paste going to the shell.
    pub fn input(&mut self, data: &[u8]) {
        // The terminal answering a query (cursor position after PSReadLine's
        // `ESC[6n`) goes the same way as keystrokes but leaves the line empty.
        if is_terminal_report(data) {
            if data.ends_with(b"R") {
                self.answered(1);
            }
            return;
        }
        if !data.is_empty() && self.at_prompt {
            self.typed = true;
        }
    }

    /// `Ok` when a hidden command may be typed now; otherwise why not.
    pub fn ready(&self) -> Result<(), &'static str> {
        if !self.seen {
            return Ok(());
        }
        if !self.at_prompt {
            return Err(
                "the terminal is busy (a program is running in it); finish it first, or use SFTP",
            );
        }
        if self.typed {
            return Err("there is text on the terminal's command line; run or clear it first");
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Phase {
    Echo,
    Payload,
    Tail,
    Done,
}

pub struct Capture {
    phase: Phase,
    /// This command's sentinels (see `nonce`).
    start: Vec<u8>,
    end: Vec<u8>,
    out: Vec<u8>,
    buf: Vec<u8>,
    payload: Vec<u8>,
    code: i32,
    started: Instant,
    last_byte: Instant,
    /// Cursor-position queries swallowed so far and not yet answered; the tail
    /// of the last swallowed chunk, for a query split across chunks.
    dsr_pending: usize,
    dsr_carry: Vec<u8>,
}

impl Capture {
    /// Capture for the command wrapped with this `nonce`.
    pub fn new(nonce: &str) -> Self {
        let now = Instant::now();
        Self {
            phase: Phase::Echo,
            start: start(nonce),
            end: end_prefix(nonce),
            out: out_prefix(nonce),
            buf: Vec::new(),
            payload: Vec::new(),
            code: 0,
            started: now,
            last_byte: now,
            dsr_pending: 0,
            dsr_carry: Vec::new(),
        }
    }

    pub fn done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Feed session output; returns the bytes the UI should still see.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let shown = self.feed_inner(bytes);
        // What is shown is always a suffix of this chunk; the rest was swallowed.
        let swallowed = &bytes[..bytes.len().saturating_sub(shown.len())];
        self.note_queries(swallowed);
        shown
    }

    /// PSReadLine on Linux/macOS asks for the cursor position (`ESC[6n`) while it
    /// redraws and waits for the answer, reading stdin until it comes. Swallowed
    /// here, the query never reaches the terminal, so nothing would answer and the
    /// shell would hang. Answer each one ourselves (`take_replies`). The position is a
    /// guess; PSReadLine asks again after the prompt the user sees, and the real
    /// terminal answers that one. (Windows ConPTY answers these itself.)
    fn note_queries(&mut self, swallowed: &[u8]) {
        if swallowed.is_empty() {
            return;
        }
        let mut buf = std::mem::take(&mut self.dsr_carry);
        buf.extend_from_slice(swallowed);
        self.dsr_pending += buf.windows(DSR.len()).filter(|w| *w == DSR).count();
        // Keep a possible query prefix, but never a whole query (counted already).
        let keep = (DSR.len() - 1).min(buf.len());
        self.dsr_carry = buf[buf.len() - keep..].to_vec();
    }

    /// Replies to the cursor-position queries swallowed since the last call, and
    /// how many (for `PromptState::answered`).
    pub fn take_replies(&mut self) -> Option<(Vec<u8>, usize)> {
        if self.dsr_pending == 0 {
            return None;
        }
        let n = std::mem::take(&mut self.dsr_pending);
        Some((DSR_REPLY.repeat(n), n))
    }

    fn feed_inner(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.last_byte = Instant::now();
        if self.phase == Phase::Done {
            return bytes.to_vec();
        }
        self.buf.extend_from_slice(bytes);
        loop {
            match self.phase {
                Phase::Echo => match find(&self.buf, &self.start) {
                    Some(i) => {
                        self.buf.drain(..i + self.start.len());
                        self.phase = Phase::Payload;
                    }
                    None => {
                        keep_tail(&mut self.buf, self.start.len() - 1);
                        return Vec::new();
                    }
                },
                Phase::Payload => match find(&self.buf, &self.end) {
                    Some(i) => {
                        // Need the exit code and its BEL before moving on.
                        let after = i + self.end.len();
                        let Some(bel) = self.buf[after..].iter().position(|&b| b == 0x07) else {
                            return Vec::new();
                        };
                        self.payload.extend_from_slice(&self.buf[..i]);
                        self.code = std::str::from_utf8(&self.buf[after..after + bel])
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(-1);
                        self.buf.drain(..after + bel + 1);
                        self.phase = Phase::Tail;
                    }
                    None => {
                        let cut = self.buf.len().saturating_sub(self.end.len() - 1);
                        self.payload.extend_from_slice(&self.buf[..cut]);
                        self.buf.drain(..cut);
                        return Vec::new();
                    }
                },
                Phase::Tail => match find(&self.buf, PROMPT_END) {
                    Some(i) => {
                        let after = i + PROMPT_END.len();
                        // Terminator: BEL, or ESC \ (two bytes).
                        let term_len = match self.buf.get(after) {
                            Some(0x07) => 1,
                            Some(0x1b) if self.buf.get(after + 1) == Some(&b'\\') => 2,
                            Some(0x1b) => return Vec::new(), // split across chunks
                            Some(_) => 1,                    // malformed; don't hang
                            None => return Vec::new(),
                        };
                        let rest = self.buf.split_off(after + term_len);
                        self.buf.clear();
                        self.phase = Phase::Done;
                        return rest;
                    }
                    None => {
                        keep_tail(&mut self.buf, PROMPT_END.len() + 1);
                        return Vec::new();
                    }
                },
                Phase::Done => return std::mem::take(&mut self.buf),
            }
        }
    }

    /// Time-based transitions: silence after the end sentinel finishes the tail
    /// swallow (no hook in this shell); a never-ending command times out.
    /// Returns `Some(Err)` on timeout, `Some(Ok)` when done, `None` while running.
    pub fn tick(&mut self) -> Option<Result<(), String>> {
        match self.phase {
            Phase::Done => Some(Ok(())),
            Phase::Tail if self.last_byte.elapsed() >= TAIL_QUIET => {
                self.phase = Phase::Done;
                self.buf.clear();
                Some(Ok(()))
            }
            _ if self.started.elapsed() >= TIMEOUT => Some(Err("hidden command timed out".into())),
            _ => None,
        }
    }

    pub fn into_result(self) -> (Vec<u8>, i32) {
        (
            out_chunks(&self.payload, &self.out).unwrap_or(self.payload),
            self.code,
        )
    }
}

/// Base64 per `7771;o` chunk (a multiple of 4, so each decodes on its own).
const OUT_CHUNK: usize = 3000;

/// The decoded `7771;o` chunks of a payload, or `None` if it has none. Anything
/// else in the payload (what the terminal repainted) is dropped.
fn out_chunks(payload: &[u8], prefix: &[u8]) -> Option<Vec<u8>> {
    let mut rest = find(payload, prefix).map(|i| &payload[i..])?;
    let mut out = Vec::new();
    while let Some(i) = find(rest, prefix) {
        rest = &rest[i + prefix.len()..];
        let end = rest.iter().position(|&b| b == 0x07).unwrap_or(rest.len());
        if let Ok(d) = base64::engine::general_purpose::STANDARD.decode(&rest[..end]) {
            out.extend(d);
        }
        rest = &rest[end..];
    }
    Some(out)
}

/// Device status report: "where is the cursor?", and the answer we give inside a capture.
const DSR: &[u8] = b"\x1b[6n";
const DSR_REPLY: &[u8] = b"\x1b[1;1R";

/// A terminal's answer to a query rather than keystrokes: a cursor position
/// (`ESC [ row ; col R`) or a device attributes report (`ESC [ ? … c`).
pub fn is_terminal_report(data: &[u8]) -> bool {
    let Some(body) = data.strip_prefix(b"\x1b[") else {
        return false;
    };
    match body.split_last() {
        Some((b'R', args)) => {
            !args.is_empty() && args.iter().all(|b| b.is_ascii_digit() || *b == b';')
        }
        Some((b'c', args)) => {
            args.first() == Some(&b'?')
                && args[1..].iter().all(|b| b.is_ascii_digit() || *b == b';')
        }
        _ => false,
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn keep_tail(buf: &mut Vec<u8>, n: usize) {
    if buf.len() > n {
        buf.drain(..buf.len() - n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap_t(cmd: &str) -> String {
        wrap(cmd, "t")
    }
    fn wrap_pwsh_t(cmd: &str) -> String {
        wrap_pwsh(cmd, "t")
    }

    #[test]
    fn output_cannot_fake_the_end_sentinel() {
        // A file being cat'ed prints sentinels without this command's nonce.
        let n = nonce();
        assert_eq!(n.len(), 16);
        assert_ne!(n, nonce(), "fresh per command");
        let mut c = Capture::new(&n);
        let s = format!("\x1b]7771;s;{n}\x07");
        c.feed(s.as_bytes());
        c.feed(b"real\r\n\x1b]7771;e;0\x07\x1b]7771;e;t;0\x07\x1b]7771;o;t;ZmFrZQ==\x07more\r\n");
        assert!(!c.done(), "forged end sentinels are payload");
        c.feed(format!("\x1b]7771;e;{n};3\x07").as_bytes());
        let (out, code) = {
            c.tick();
            c.into_result()
        };
        assert_eq!(code, 3);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("real\r\n") && text.ends_with("more\r\n"),
            "{text:?}"
        );
        assert!(!text.contains("fake"), "no forged 7771;o chunk decoded");
        // The wrapped line carries the nonce in all three sentinels.
        let w = wrap("ls", &n);
        assert!(w.contains(&format!("7771;s;{n}")) && w.contains(&format!("7771;e;{n};")));
        let p = wrap_pwsh("ls", &n);
        assert!(
            p.contains(&format!("7771;s;{n}"))
                && p.contains(&format!("7771;o;{n};"))
                && p.contains(&format!("7771;e;{n};"))
        );
    }

    #[test]
    fn quoting_and_wrapping() {
        assert_eq!(sq("a b"), "'a b'");
        assert_eq!(sq("it's"), "'it'\\''s'");
        let w = wrap_t("ls");
        assert!(w.starts_with("\x15 printf"));
        assert!(w.ends_with("\"$?\"\n"));
    }

    #[test]
    fn dialect_follows_the_shell() {
        assert_eq!(Dialect::of("bash"), Some(Dialect::Posix));
        assert_eq!(Dialect::of("/usr/bin/zsh"), Some(Dialect::Posix));
        assert_eq!(Dialect::of("pwsh"), Some(Dialect::Pwsh));
        assert_eq!(
            Dialect::of(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            Some(Dialect::Pwsh)
        );
        assert_eq!(
            Dialect::of(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"),
            Some(Dialect::Pwsh)
        );
        assert_eq!(Dialect::of("fish"), None);
        assert_eq!(Dialect::of(r"C:\WINDOWS\system32\cmd.exe"), None);
        assert_eq!(Dialect::of("sudo"), None);
    }

    #[test]
    fn pwsh_wrap_is_one_line_carrying_the_command_in_base64() {
        let cmd = "Get-ChildItem 'C:\\it''s here'\n| Select -First 1";
        let w = wrap_pwsh_t(cmd);
        assert!(w.starts_with(' '), "leading space, as the hook injection");
        assert!(w.ends_with('\r'), "Enter");
        assert_eq!(
            w.matches(['\r', '\n']).count(),
            1,
            "exactly one line: {w:?}"
        );
        assert!(
            !w.contains('\x1b'),
            "no raw ESC: the echo must not look like a sentinel"
        );
        let b64 = w
            .split("FromBase64String('")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), cmd);
    }

    #[test]
    fn output_chunks_replace_what_was_painted() {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let (a, b) = (b64("line one\r\n"), b64("two\r\n"));
        // ConPTY repaint noise around the chunks is dropped; chunks join in order.
        let mut c = Capture::new("t");
        c.feed(b"echo\x1b]7771;s;t\x07\x1b[?25l");
        c.feed(format!("\x1b]7771;o;t;{a}\x07\x1b[H\x1b]7771;o;t;").as_bytes());
        c.feed(format!("{b}\x07\x1b]7771;e;t;0\x07").as_bytes());
        assert_eq!(c.into_result(), (b"line one\r\ntwo\r\n".to_vec(), 0));
        // No chunks (POSIX shells): the payload as printed.
        let mut c = Capture::new("t");
        c.feed(b"\x1b]7771;s;t\x07plain\r\n\x1b]7771;e;t;0\x07");
        assert_eq!(c.into_result().0, b"plain\r\n");
        // A command with no output still sends nothing, and gives nothing.
        assert_eq!(out_chunks(b"\x1b[?25h", &out_prefix("t")), None);
    }

    #[test]
    fn pwsh_exit_code_mapping() {
        let w = wrap_pwsh_t("x");
        // $LASTEXITCODE is cleared before the command, so a nonzero value is its own.
        assert!(w.contains("$global:LASTEXITCODE = 0;"));
        let reset = w.find("$global:LASTEXITCODE = 0").unwrap();
        assert!(reset < w.find("Invoke-Expression").unwrap());
        // Native exit code first, then $?/$Error (the hook's 133;D order: success → 0, else 1).
        assert!(w.contains("$(if ($global:LASTEXITCODE) { $global:LASTEXITCODE } elseif ($__charon_ok) { 0 } else { 1 })"), "{w}");
        // A terminating error is caught, still reaching the end sentinel as a failure.
        assert!(w.contains("catch { $__charon_ok = $false;"));
        // Output is collected and sent in the sentinel stream, not drawn.
        assert!(w.contains("*>&1 | Out-String; $__charon_ok = $?"), "{w}");
        assert!(w.contains("]7771;o;"), "{w}");
        // What PowerShell prints comes through the same Capture as POSIX shells.
        let mut c = Capture::new("t");
        assert!(
            c.feed(w.replace('\r', "\r\n").as_bytes()).is_empty(),
            "echo swallowed"
        );
        assert!(c
            .feed(b"\x1b]7771;s;t\x07charon\r\n\x1b]7771;e;t;3\x07")
            .is_empty());
        let shown = c.feed(b"\x1b]133;D;1\x07\x1b]133;A\x07PS C:\\> \x1b]133;B\x07");
        assert!(shown.is_empty() && c.done());
        assert_eq!(c.into_result(), (b"charon\r\n".to_vec(), 3));
    }

    #[test]
    fn swallowed_cursor_queries_are_answered_but_shown_ones_are_left_to_the_terminal() {
        let mut c = Capture::new("t");
        // PSReadLine's queries inside the window, one split across chunks.
        assert!(c
            .feed(b"echo...\x1b[6n\x1b]7771;s;t\x07out\x1b[")
            .is_empty());
        assert!(c.feed(b"6n\x1b]7771;e;t;0\x07\x1b[?1h\x1b[6n").is_empty());
        assert_eq!(
            c.take_replies(),
            Some((b"\x1b[1;1R\x1b[1;1R\x1b[1;1R".to_vec(), 3))
        );
        assert_eq!(c.take_replies(), None, "answered once");
        // The query after the visible prompt reaches the terminal, which answers it.
        let shown = c.feed(b"\x1b]133;B\x07\x1b[?1h\x1b[6n");
        assert_eq!(shown, b"\x1b[?1h\x1b[6n");
        assert_eq!(c.take_replies(), None);
    }

    #[test]
    fn prompt_state_tracks_unanswered_cursor_queries() {
        let mut p = PromptState::default();
        p.output(b"\x1b]133;B\x07\x1b[?1h\x1b[");
        assert!(!p.awaiting_report());
        p.output(b"6n");
        assert!(p.awaiting_report(), "query split across chunks");
        p.input(b"\x1b[3;5R");
        assert!(!p.awaiting_report(), "the terminal answered");
        p.output(b"\x1b[6n\x1b[6n");
        p.answered(2);
        assert!(!p.awaiting_report(), "answered by the capture");
        p.input(b"\x1b[1;1R");
        assert!(!p.awaiting_report(), "an extra answer doesn't underflow");
    }

    #[test]
    fn a_terminal_report_is_not_typing() {
        assert!(is_terminal_report(b"\x1b[12;40R"));
        assert!(is_terminal_report(b"\x1b[?1;2c"));
        assert!(!is_terminal_report(b"\x1b[A"), "arrow up is typing");
        assert!(!is_terminal_report(b"R"));
        assert!(!is_terminal_report(b"ls\r"));
        let mut p = PromptState::default();
        p.output(b"\x1b]133;A\x07PS> \x1b]133;B\x07\x1b[6n");
        p.input(b"\x1b[3;5R");
        assert!(p.ready().is_ok(), "the cursor report left the line empty");
    }

    #[test]
    fn uac_triggers_are_refused_before_typing() {
        assert_eq!(uac_trigger("sudo winget install foo"), Some("sudo"));
        assert_eq!(uac_trigger("gsudo.exe whoami"), Some("gsudo"));
        assert_eq!(
            uac_trigger("Start-Process notepad -Verb RunAs"),
            Some("RunAs")
        );
        assert_eq!(uac_trigger("saps pwsh -Verb:runas"), Some("RunAs"));
        assert_eq!(uac_trigger("runas /user:Administrator cmd"), Some("RunAs"));
        assert_eq!(uac_trigger("Get-ChildItem C:\\Windows"), None);
        assert_eq!(uac_trigger("echo pseudonym; ./sudoku.exe"), None);
    }

    #[test]
    fn multi_line_command_puts_the_end_sentinel_after_the_heredoc() {
        let w = wrap_t("base64 -d <<'X' > f\naGkK\nX");
        let lines: Vec<&str> = w.lines().collect();
        assert!(
            lines[0].starts_with("\x15 printf '\\033]7771;s;t\\a'; { base64 -d"),
            "{w:?}"
        );
        // The terminator stands alone; the end sentinel follows on its own line.
        assert_eq!(lines[2], "X");
        assert!(lines[3].starts_with("}; printf '\\033]7771;e;t;"), "{w:?}");
    }

    #[test]
    fn prompt_state_allows_hidden_commands_only_at_an_empty_prompt() {
        let mut p = PromptState::default();
        assert!(
            p.ready().is_ok(),
            "no shell integration: nothing to go on, allowed"
        );
        p.output(b"\x1b]133;A\x07user@h:~$ \x1b]133;B\x07");
        assert!(p.ready().is_ok(), "idle prompt");
        p.input(b"df -");
        assert!(
            p.ready().unwrap_err().contains("command line"),
            "a half-typed line would be erased"
        );
        p.input(b"h\r");
        p.output(b"df -h\r\n\x1b]133;C\x07Filesystem ...\r\n");
        assert!(p.ready().unwrap_err().contains("busy"), "command running");
        p.output(b"\x1b]133;D;0\x07\x1b]133;A\x07user@h:~$ \x1b]133;B\x07");
        assert!(p.ready().is_ok(), "back at an empty prompt");
        // vim owns the terminal from its 133;C until it exits.
        p.input(b"vim notes\r");
        p.output(b"\x1b]133;C\x07\x1b[?1049h");
        p.input(b"ihello");
        assert!(p.ready().is_err(), "never type into vim");
    }

    #[test]
    fn prompt_state_finds_a_mark_split_across_chunks() {
        let mut p = PromptState::default();
        p.output(b"out\x1b]13");
        p.output(b"3;C\x07more");
        assert!(p.ready().unwrap_err().contains("busy"));
        p.output(b"\x1b]133;");
        p.output(b"B\x07");
        assert!(p.ready().is_ok());
    }

    #[test]
    fn swallows_echo_collects_payload_and_restores_after_prompt() {
        let mut c = Capture::new("t");
        // Echo of the typed line (contains the *text* of the sentinel, not the bytes), then real output.
        assert!(c
            .feed(b" printf '\\033]7771;s;t\\a'; ls; printf ...\r\n")
            .is_empty());
        assert!(c.feed(b"\x1b]7771;s;t\x07hello\r\nworld\r\n").is_empty());
        assert!(!c.done());
        assert!(c.feed(b"\x1b]7771;e;t;0\x07").is_empty());
        // Redrawn prompt with hook marks is swallowed up to and including 133;B; what follows is shown.
        let shown = c.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07root@h:~# \x1b]133;B\x07typed");
        assert_eq!(shown, b"typed");
        assert!(c.done());
        let (payload, code) = c.into_result();
        assert_eq!(payload, b"hello\r\nworld\r\n");
        assert_eq!(code, 0);
    }

    #[test]
    fn sentinels_split_across_chunks_and_nonzero_exit() {
        let mut c = Capture::new("t");
        c.feed(b"echo\r\n\x1b]77");
        c.feed(b"71;s;t\x07out");
        c.feed(b"put\x1b]7771;e;t;");
        assert!(!c.done());
        c.feed(b"2\x07");
        assert_eq!(c.tick(), None); // tail: waiting for prompt or silence
        std::thread::sleep(TAIL_QUIET + Duration::from_millis(20));
        assert_eq!(c.tick(), Some(Ok(())));
        assert_eq!(c.into_result(), (b"output".to_vec(), 2));
    }

    #[test]
    fn strips_133c_and_colour_inside_the_window() {
        // Real framing: preexec 133;C before the pwd, another before the end printf.
        let mut c = Capture::new("t");
        assert!(c.feed(b" printf ...s...\r\n\x1b]133;C\x07").is_empty()); // echo + preexec, pre-start
        assert!(c.feed(b"\x1b]7771;s;t\x07").is_empty());
        assert!(c.feed(b"\x1b]133;C\x07/home/admin\r\n").is_empty()); // preexec + output inside
        assert!(c.feed(b"\x1b]133;C\x07").is_empty()); // preexec before end printf
        assert!(c.feed(b"\x1b]7771;e;t;0\x07").is_empty());
        let shown = c.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07admin@devbox:~$ \x1b]133;B\x07next");
        assert_eq!(shown, b"next");
        let (payload, code) = c.into_result();
        assert_eq!(code, 0);
        assert_eq!(
            strip_escapes(&String::from_utf8_lossy(&payload)),
            "/home/admin\r\n"
        );
    }

    #[test]
    fn strip_handles_osc_csi_and_st() {
        assert_eq!(strip_escapes("\x1b]133;C\x07/root"), "/root");
        assert_eq!(strip_escapes("a\x1b[0mb\x1b[1;32mc"), "abc");
        assert_eq!(strip_escapes("x\x1b]0;title\x1b\\y"), "xy"); // OSC with ST terminator
        assert_eq!(strip_escapes("plain"), "plain");
    }

    #[test]
    fn root_commands_are_split_off_and_checked() {
        assert_eq!(root_command("uptime"), Ok(None));
        assert_eq!(root_command("echo pseudonym; ./sudoku"), Ok(None));
        assert_eq!(root_command("sudo lsblk"), Ok(Some("lsblk".into())));
        assert_eq!(
            root_command("  sudo  systemctl restart nginx "),
            Ok(Some("systemctl restart nginx".into()))
        );
        assert_eq!(
            root_command("sudo apt update && apt upgrade -y"),
            Ok(Some("apt update && apt upgrade -y".into()))
        );
        assert!(root_command("sudo").is_err());
        assert!(root_command("sudo -u postgres psql").is_err(), "options");
        assert!(
            root_command("sudo ls; sudo reboot").is_err(),
            "a second sudo"
        );
        assert!(root_command("sudo su -").is_err());
        assert!(root_command("sudo sh -c 'su root'").is_err());
        assert!(
            root_command("cd /etc && sudo cat shadow").is_err(),
            "sudo not at the start"
        );
        assert!(root_command("echo x | su").is_err());
        assert!(root_command("doas reboot").is_err());
    }

    #[test]
    fn root_exec_line() {
        assert_eq!(
            root_exec("lsblk", true),
            "sudo -S -k -p '' -- sh -c 'exec </dev/null; lsblk'"
        );
        assert_eq!(
            root_exec("echo hi", false),
            "sudo -n -k -- sh -c 'exec </dev/null; echo hi'"
        );
        // The script is quoted for sh -c: a quote in it can't end the argument.
        assert_eq!(
            root_exec("echo a'b", true),
            format!(
                "sudo -S -k -p '' -- sh -c {}",
                sq("exec </dev/null; echo a'b")
            )
        );
    }

    #[test]
    fn prompt_end_with_st_terminator_split() {
        let mut c = Capture::new("t");
        c.feed(b"\x1b]7771;s;t\x07x\x1b]7771;e;t;0\x07\x1b]133;B\x1b");
        assert!(!c.done());
        assert_eq!(c.feed(b"\\after"), b"after");
        assert!(c.done());
    }
}
