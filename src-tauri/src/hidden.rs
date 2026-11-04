//! Hidden commands on an interactive shell. The line is typed into the session
//! wrapped in private OSC sentinels; the session's output task feeds every byte
//! through `Capture`, which swallows the echo, collects the payload between the
//! sentinels, then keeps swallowing the redrawn prompt (up to the next `133;B`
//! mark or a short silence) so the user's screen does not change at all.
//!
//! Pure state machine, tested; the SSH task and the local PTY's emitter thread
//! drive it. POSIX shells get `wrap`, PowerShell gets `wrap_pwsh`; both print the
//! same sentinels, so one `Capture` serves every shell.

use base64::Engine;
use std::time::{Duration, Instant};

/// `ESC ] 7771 ; s BEL` — payload starts after this.
pub const START: &[u8] = b"\x1b]7771;s\x07";
/// `ESC ] 7771 ; e ; <exit code> BEL` — payload ends before this.
pub const END_PREFIX: &[u8] = b"\x1b]7771;e;";
/// `ESC ] 7771 ; o ; <base64> BEL` — a piece of the output, sent inside the
/// sentinel stream (PowerShell). When a payload has any, they are the output.
pub const OUT_PREFIX: &[u8] = b"\x1b]7771;o;";
/// `ESC ] 133 ; B` (BEL or ESC \ terminated) — prompt redrawn, screen state restored.
const PROMPT_END: &[u8] = b"\x1b]133;B";
/// No hook in this shell: after the end sentinel, this much silence ends the tail swallow.
pub const TAIL_QUIET: Duration = Duration::from_millis(300);
/// Give up on a command that never produces its end sentinel.
pub const TIMEOUT: Duration = Duration::from_secs(120);

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
/// whatever is typed next.
pub fn wrap(cmd: &str) -> String {
    let end = "printf '\\033]7771;e;%s\\a' \"$?\"";
    if cmd.contains('\n') {
        format!("\x15 printf '\\033]7771;s\\a'; {{ {cmd}\n}}; {end}\n")
    } else {
        format!("\x15 printf '\\033]7771;s\\a'; {cmd}; {end}\n")
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
        let base = shell.rsplit(['/', '\\']).next().unwrap_or(shell).to_ascii_lowercase();
        match base.strip_suffix(".exe").unwrap_or(&base) {
            "bash" | "zsh" | "sh" | "dash" | "ksh" | "mksh" | "ash" => Some(Dialect::Posix),
            "pwsh" | "pwsh-preview" | "powershell" => Some(Dialect::Pwsh),
            _ => None,
        }
    }

    pub fn wrap(self, cmd: &str) -> String {
        match self {
            Dialect::Posix => wrap(cmd),
            Dialect::Pwsh => wrap_pwsh(cmd),
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
pub fn wrap_pwsh(cmd: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
    let esc = "$([char]27)";
    let bel = "$([char]7)";
    format!(
        " [Console]::Write(\"{esc}]7771;s{bel}\"); $global:LASTEXITCODE = 0; $__charon_e = $Error[0]; \
$__charon_o = ''; \
try {{ $__charon_o = Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b64}'))) *>&1 | Out-String; $__charon_ok = $? }} \
catch {{ $__charon_ok = $false; $__charon_o += $_ | Out-String }}; \
$__charon_b = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$__charon_o)); \
for ($__charon_i = 0; $__charon_i -lt $__charon_b.Length; $__charon_i += {OUT_CHUNK}) {{ \
[Console]::Write(\"{esc}]7771;o;\" + $__charon_b.Substring($__charon_i, [Math]::Min({OUT_CHUNK}, $__charon_b.Length - $__charon_i)) + \"{bel}\") }}; \
if ($Error.Count -and -not [object]::ReferenceEquals($Error[0], $__charon_e)) {{ $__charon_ok = $false }}; \
[Console]::Write(\"{esc}]7771;e;$(if ($global:LASTEXITCODE) {{ $global:LASTEXITCODE }} elseif ($__charon_ok) {{ 0 }} else {{ 1 }}){bel}\")\r"
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
            return Err("the terminal is busy (a program is running in it); finish it first, or use SFTP");
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
    buf: Vec<u8>,
    payload: Vec<u8>,
    code: i32,
    started: Instant,
    last_byte: Instant,
    /// Password to answer a `[sudo] password for <login-user>:` prompt with, so an
    /// agent's `sudo …` (MCP run_command) doesn't hang. Never logged. Auto-answer
    /// is deliberately restricted to the login user's own sudo prompt — never a
    /// bare `Password:` (su), which stays manual.
    sudo_pw: Option<String>,
    sudo_user: String,
    sudo_sent: bool,
    /// Rolling tail of recent output, for prompt sniffing across chunk boundaries.
    sniff: Vec<u8>,
    /// Cursor-position queries swallowed so far and not yet answered; the tail
    /// of the last swallowed chunk, for a query split across chunks.
    dsr_pending: usize,
    dsr_carry: Vec<u8>,
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { phase: Phase::Echo, buf: Vec::new(), payload: Vec::new(), code: 0, started: now, last_byte: now, sudo_pw: None, sudo_user: String::new(), sudo_sent: false, sniff: Vec::new(), dsr_pending: 0, dsr_carry: Vec::new() }
    }

    /// Arm auto-answering of the login user's own `[sudo] password for <user>:`
    /// prompt seen during capture. A bare `Password:` (su) is never auto-answered.
    pub fn set_sudo_password(&mut self, pw: String, login_user: String) {
        self.sudo_pw = Some(pw);
        self.sudo_user = login_user;
    }

    /// If the login user's sudo prompt is waiting and a password is armed, return
    /// the bytes to send (password + newline) — once. The caller writes them to the
    /// channel and must never log them.
    pub fn take_password_send(&mut self) -> Option<Vec<u8>> {
        if self.sudo_sent {
            return None;
        }
        let pw = self.sudo_pw.as_ref()?;
        if sniff_sudo_prompt_for(&self.sniff, &self.sudo_user) {
            self.sudo_sent = true;
            let mut v = pw.clone().into_bytes();
            v.push(b'\n');
            return Some(v);
        }
        None
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
        // Keep a short tail of raw output for sudo-prompt sniffing.
        if self.sudo_pw.is_some() && !self.sudo_sent {
            self.sniff.extend_from_slice(bytes);
            keep_tail(&mut self.sniff, 256);
        }
        if self.phase == Phase::Done {
            return bytes.to_vec();
        }
        self.buf.extend_from_slice(bytes);
        loop {
            match self.phase {
                Phase::Echo => match find(&self.buf, START) {
                    Some(i) => {
                        self.buf.drain(..i + START.len());
                        self.phase = Phase::Payload;
                    }
                    None => {
                        keep_tail(&mut self.buf, START.len() - 1);
                        return Vec::new();
                    }
                },
                Phase::Payload => match find(&self.buf, END_PREFIX) {
                    Some(i) => {
                        // Need the exit code and its BEL before moving on.
                        let after = i + END_PREFIX.len();
                        let Some(bel) = self.buf[after..].iter().position(|&b| b == 0x07) else { return Vec::new() };
                        self.payload.extend_from_slice(&self.buf[..i]);
                        self.code = std::str::from_utf8(&self.buf[after..after + bel]).ok().and_then(|s| s.parse().ok()).unwrap_or(-1);
                        self.buf.drain(..after + bel + 1);
                        self.phase = Phase::Tail;
                    }
                    None => {
                        let cut = self.buf.len().saturating_sub(END_PREFIX.len() - 1);
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
        (out_chunks(&self.payload).unwrap_or(self.payload), self.code)
    }
}

/// Base64 per `7771;o` chunk (a multiple of 4, so each decodes on its own).
const OUT_CHUNK: usize = 3000;

/// The decoded `7771;o` chunks of a payload, or `None` if it has none. Anything
/// else in the payload (what the terminal repainted) is dropped.
fn out_chunks(payload: &[u8]) -> Option<Vec<u8>> {
    let mut rest = find(payload, OUT_PREFIX).map(|i| &payload[i..])?;
    let mut out = Vec::new();
    while let Some(i) = find(rest, OUT_PREFIX) {
        rest = &rest[i + OUT_PREFIX.len()..];
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
    let Some(body) = data.strip_prefix(b"\x1b[") else { return false };
    match body.split_last() {
        Some((b'R', args)) => !args.is_empty() && args.iter().all(|b| b.is_ascii_digit() || *b == b';'),
        Some((b'c', args)) => args.first() == Some(&b'?') && args[1..].iter().all(|b| b.is_ascii_digit() || *b == b';'),
        _ => false,
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// True when the tail of recent output is *the login user's own* sudo prompt,
/// `[sudo] password for <login_user>:`. Deliberately strict: a prompt for another
/// user, or a bare `Password:` (su), is not auto-answered. `login_user` empty ⇒ never.
pub fn sniff_sudo_prompt_for(buf: &[u8], login_user: &str) -> bool {
    if login_user.is_empty() {
        return false;
    }
    let s = strip_escapes(&String::from_utf8_lossy(buf));
    s.trim_end().ends_with(&format!("[sudo] password for {login_user}:"))
}

fn keep_tail(buf: &mut Vec<u8>, n: usize) {
    if buf.len() > n {
        buf.drain(..buf.len() - n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_wrapping() {
        assert_eq!(sq("a b"), "'a b'");
        assert_eq!(sq("it's"), "'it'\\''s'");
        let w = wrap("ls");
        assert!(w.starts_with("\x15 printf"));
        assert!(w.ends_with("\"$?\"\n"));
    }

    #[test]
    fn dialect_follows_the_shell() {
        assert_eq!(Dialect::of("bash"), Some(Dialect::Posix));
        assert_eq!(Dialect::of("/usr/bin/zsh"), Some(Dialect::Posix));
        assert_eq!(Dialect::of("pwsh"), Some(Dialect::Pwsh));
        assert_eq!(Dialect::of(r"C:\Program Files\PowerShell\7\pwsh.exe"), Some(Dialect::Pwsh));
        assert_eq!(Dialect::of(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"), Some(Dialect::Pwsh));
        assert_eq!(Dialect::of("fish"), None);
        assert_eq!(Dialect::of(r"C:\WINDOWS\system32\cmd.exe"), None);
        assert_eq!(Dialect::of("sudo"), None);
    }

    #[test]
    fn pwsh_wrap_is_one_line_carrying_the_command_in_base64() {
        let cmd = "Get-ChildItem 'C:\\it''s here'\n| Select -First 1";
        let w = wrap_pwsh(cmd);
        assert!(w.starts_with(' '), "leading space, as the hook injection");
        assert!(w.ends_with('\r'), "Enter");
        assert_eq!(w.matches(['\r', '\n']).count(), 1, "exactly one line: {w:?}");
        assert!(!w.contains('\x1b'), "no raw ESC: the echo must not look like a sentinel");
        let b64 = w.split("FromBase64String('").nth(1).unwrap().split('\'').next().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), cmd);
    }

    #[test]
    fn output_chunks_replace_what_was_painted() {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let (a, b) = (b64("line one\r\n"), b64("two\r\n"));
        // ConPTY repaint noise around the chunks is dropped; chunks join in order.
        let mut c = Capture::new();
        c.feed(b"echo\x1b]7771;s\x07\x1b[?25l");
        c.feed(format!("\x1b]7771;o;{a}\x07\x1b[H\x1b]7771;o;").as_bytes());
        c.feed(format!("{b}\x07\x1b]7771;e;0\x07").as_bytes());
        assert_eq!(c.into_result(), (b"line one\r\ntwo\r\n".to_vec(), 0));
        // No chunks (POSIX shells): the payload as printed.
        let mut c = Capture::new();
        c.feed(b"\x1b]7771;s\x07plain\r\n\x1b]7771;e;0\x07");
        assert_eq!(c.into_result().0, b"plain\r\n");
        // A command with no output still sends nothing, and gives nothing.
        assert_eq!(out_chunks(b"\x1b[?25h"), None);
    }

    #[test]
    fn pwsh_exit_code_mapping() {
        let w = wrap_pwsh("x");
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
        let mut c = Capture::new();
        assert!(c.feed(w.replace('\r', "\r\n").as_bytes()).is_empty(), "echo swallowed");
        assert!(c.feed(b"\x1b]7771;s\x07charon\r\n\x1b]7771;e;3\x07").is_empty());
        let shown = c.feed(b"\x1b]133;D;1\x07\x1b]133;A\x07PS C:\\> \x1b]133;B\x07");
        assert!(shown.is_empty() && c.done());
        assert_eq!(c.into_result(), (b"charon\r\n".to_vec(), 3));
    }

    #[test]
    fn swallowed_cursor_queries_are_answered_but_shown_ones_are_left_to_the_terminal() {
        let mut c = Capture::new();
        // PSReadLine's queries inside the window, one split across chunks.
        assert!(c.feed(b"echo...\x1b[6n\x1b]7771;s\x07out\x1b[").is_empty());
        assert!(c.feed(b"6n\x1b]7771;e;0\x07\x1b[?1h\x1b[6n").is_empty());
        assert_eq!(c.take_replies(), Some((b"\x1b[1;1R\x1b[1;1R\x1b[1;1R".to_vec(), 3)));
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
        assert_eq!(uac_trigger("Start-Process notepad -Verb RunAs"), Some("RunAs"));
        assert_eq!(uac_trigger("saps pwsh -Verb:runas"), Some("RunAs"));
        assert_eq!(uac_trigger("runas /user:Administrator cmd"), Some("RunAs"));
        assert_eq!(uac_trigger("Get-ChildItem C:\\Windows"), None);
        assert_eq!(uac_trigger("echo pseudonym; ./sudoku.exe"), None);
    }

    #[test]
    fn multi_line_command_puts_the_end_sentinel_after_the_heredoc() {
        let w = wrap("base64 -d <<'X' > f\naGkK\nX");
        let lines: Vec<&str> = w.lines().collect();
        assert!(lines[0].starts_with("\x15 printf '\\033]7771;s\\a'; { base64 -d"), "{w:?}");
        // The terminator stands alone; the end sentinel follows on its own line.
        assert_eq!(lines[2], "X");
        assert!(lines[3].starts_with("}; printf '\\033]7771;e;"), "{w:?}");
    }

    #[test]
    fn prompt_state_allows_hidden_commands_only_at_an_empty_prompt() {
        let mut p = PromptState::default();
        assert!(p.ready().is_ok(), "no shell integration: nothing to go on, allowed");
        p.output(b"\x1b]133;A\x07user@h:~$ \x1b]133;B\x07");
        assert!(p.ready().is_ok(), "idle prompt");
        p.input(b"df -");
        assert!(p.ready().unwrap_err().contains("command line"), "a half-typed line would be erased");
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
        let mut c = Capture::new();
        // Echo of the typed line (contains the *text* of the sentinel, not the bytes), then real output.
        assert!(c.feed(b" printf '\\033]7771;s\\a'; ls; printf ...\r\n").is_empty());
        assert!(c.feed(b"\x1b]7771;s\x07hello\r\nworld\r\n").is_empty());
        assert!(!c.done());
        assert!(c.feed(b"\x1b]7771;e;0\x07").is_empty());
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
        let mut c = Capture::new();
        c.feed(b"echo\r\n\x1b]77");
        c.feed(b"71;s\x07out");
        c.feed(b"put\x1b]7771;e;");
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
        let mut c = Capture::new();
        assert!(c.feed(b" printf ...s...\r\n\x1b]133;C\x07").is_empty()); // echo + preexec, pre-start
        assert!(c.feed(b"\x1b]7771;s\x07").is_empty());
        assert!(c.feed(b"\x1b]133;C\x07/home/admin\r\n").is_empty()); // preexec + output inside
        assert!(c.feed(b"\x1b]133;C\x07").is_empty()); // preexec before end printf
        assert!(c.feed(b"\x1b]7771;e;0\x07").is_empty());
        let shown = c.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07admin@devbox:~$ \x1b]133;B\x07next");
        assert_eq!(shown, b"next");
        let (payload, code) = c.into_result();
        assert_eq!(code, 0);
        assert_eq!(strip_escapes(&String::from_utf8_lossy(&payload)), "/home/admin\r\n");
    }

    #[test]
    fn strip_handles_osc_csi_and_st() {
        assert_eq!(strip_escapes("\x1b]133;C\x07/root"), "/root");
        assert_eq!(strip_escapes("a\x1b[0mb\x1b[1;32mc"), "abc");
        assert_eq!(strip_escapes("x\x1b]0;title\x1b\\y"), "xy"); // OSC with ST terminator
        assert_eq!(strip_escapes("plain"), "plain");
    }

    #[test]
    fn sniffs_only_the_login_users_sudo_prompt() {
        assert!(sniff_sudo_prompt_for(b"[sudo] password for admin: ", "admin"));
        assert!(sniff_sudo_prompt_for(b"noise\r\n[sudo] password for admin:", "admin"));
        // A different user, a bare su prompt, or an unknown login user: never.
        assert!(!sniff_sudo_prompt_for(b"[sudo] password for root:", "admin"));
        assert!(!sniff_sudo_prompt_for(b"foo\r\nPassword:", "admin"));
        assert!(!sniff_sudo_prompt_for(b"[sudo] password for admin:", ""));
        assert!(!sniff_sudo_prompt_for(b"admin@host:~$ ", "admin"));
    }

    #[test]
    fn capture_answers_the_login_users_sudo_prompt_once() {
        let mut c = Capture::new();
        c.set_sudo_password("hunter2".into(), "admin".into());
        // Prompt for the login user arrives inside the window: password offered once.
        c.feed(b"\x1b]7771;s\x07[sudo] password for admin: ");
        let send = c.take_password_send().expect("password to send");
        assert_eq!(send, b"hunter2\n");
        assert!(c.take_password_send().is_none()); // never twice
    }

    #[test]
    fn capture_ignores_su_and_other_user_prompts() {
        // Bare su `Password:` — never auto-answered (manual button only).
        let mut c = Capture::new();
        c.set_sudo_password("hunter2".into(), "admin".into());
        c.feed(b"\x1b]7771;s\x07Password: ");
        assert!(c.take_password_send().is_none());
        // sudo for a different user — never.
        let mut c2 = Capture::new();
        c2.set_sudo_password("hunter2".into(), "admin".into());
        c2.feed(b"\x1b]7771;s\x07[sudo] password for postgres: ");
        assert!(c2.take_password_send().is_none());
    }

    #[test]
    fn capture_without_prompt_sends_nothing() {
        let mut c = Capture::new();
        c.set_sudo_password("hunter2".into(), "admin".into());
        c.feed(b"\x1b]7771;s\x07regular output\r\n");
        assert!(c.take_password_send().is_none());
    }

    #[test]
    fn prompt_end_with_st_terminator_split() {
        let mut c = Capture::new();
        c.feed(b"\x1b]7771;s\x07x\x1b]7771;e;0\x07\x1b]133;B\x1b");
        assert!(!c.done());
        assert_eq!(c.feed(b"\\after"), b"after");
        assert!(c.done());
    }
}
