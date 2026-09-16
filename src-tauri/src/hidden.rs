//! Hidden commands on an interactive shell. The line is typed into the session
//! wrapped in private OSC sentinels; the session's output task feeds every byte
//! through `Capture`, which swallows the echo, collects the payload between the
//! sentinels, then keeps swallowing the redrawn prompt (up to the next `133;B`
//! mark or a short silence) so the user's screen does not change at all.
//!
//! Pure state machine, tested; the SSH task drives it.

use std::time::{Duration, Instant};

/// `ESC ] 7771 ; s BEL` — payload starts after this.
pub const START: &[u8] = b"\x1b]7771;s\x07";
/// `ESC ] 7771 ; e ; <exit code> BEL` — payload ends before this.
pub const END_PREFIX: &[u8] = b"\x1b]7771;e;";
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

/// The keystrokes for one hidden command. Ctrl+U first clears anything the user
/// had half-typed at the prompt; the leading space keeps it out of history.
pub fn wrap(cmd: &str) -> String {
    format!("\x15 printf '\\033]7771;s\\a'; {cmd}; printf '\\033]7771;e;%s\\a' \"$?\"\n")
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
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { phase: Phase::Echo, buf: Vec::new(), payload: Vec::new(), code: 0, started: now, last_byte: now }
    }

    pub fn done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Feed session output; returns the bytes the UI should still see.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.last_byte = Instant::now();
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
        (self.payload, self.code)
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

    #[test]
    fn quoting_and_wrapping() {
        assert_eq!(sq("a b"), "'a b'");
        assert_eq!(sq("it's"), "'it'\\''s'");
        let w = wrap("ls");
        assert!(w.starts_with("\x15 printf"));
        assert!(w.ends_with("\"$?\"\n"));
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
        let shown = c.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07admin@super-earth:~$ \x1b]133;B\x07next");
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
    fn prompt_end_with_st_terminator_split() {
        let mut c = Capture::new();
        c.feed(b"\x1b]7771;s\x07x\x1b]7771;e;0\x07\x1b]133;B\x1b");
        assert!(!c.done());
        assert_eq!(c.feed(b"\\after"), b"after");
        assert!(c.done());
    }
}
