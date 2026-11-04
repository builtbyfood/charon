//! Local shell in a PTY via portable-pty (ConPTY on Windows, openpty elsewhere).
//! Blocking reads/waits run on plain threads; the `Session` methods are cheap.
//! Hidden commands (MCP run_command) work as on SSH: the emitter thread feeds
//! output through `hidden::Capture` while one runs (see `exec_hidden_sudo`).

use crate::session::{Session, SessionManager};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tokio::sync::oneshot;

pub struct LocalParams {
    /// Program to run; `None` picks the platform default (see `default_shell`).
    pub shell: Option<String>,
    /// Extra arguments (tests use `sh -c` / `cmd /c`).
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Stored password offered (never shown) to a sudo/su prompt in this shell, on
    /// Unix. From the node's credential or the `local.credential` profile setting.
    pub sudo_password: Option<String>,
    /// The shell's name when `shell` is a wrapper (`sudo -i` for Local (admin));
    /// None = the program itself. Picks the hidden-command syntax until the UI
    /// detects the shell.
    pub shell_name: Option<String>,
}

type Reply = oneshot::Sender<Result<(Vec<u8>, i32)>>;

/// What the emitter thread works through, in order: PTY output, and hidden
/// commands to start (queued behind the output already read, as on SSH).
enum Ev {
    Data(Vec<u8>),
    /// Type `line` and capture its output; `sudo` = (password, login user) to
    /// auto-answer that user's sudo prompt with (MCP, tier-gated).
    Capture { line: Vec<u8>, sudo: Option<(String, String)>, reply: Reply },
    Eof,
}

/// Shared by `write` (the user's keystrokes) and the emitter thread.
#[derive(Default)]
struct Typing {
    /// Prompt / busy state from the OSC 133 marks (hidden commands only at an empty prompt).
    prompt: crate::hidden::PromptState,
    /// A hidden command is capturing: keystrokes wait in `pending` until it ends.
    capturing: bool,
    pending: Vec<u8>,
}

pub struct LocalSession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Basename of what was launched, for the tab title.
    pub program: String,
    /// Stored sudo password (Unix), offered to a sudo/su prompt; never shown.
    /// Read only under cfg(unix); harmless and unused on Windows.
    #[cfg_attr(not(unix), allow(dead_code))]
    sudo_password: Option<String>,
    typing: Arc<Mutex<Typing>>,
    events: Mutex<std::sync::mpsc::Sender<Ev>>,
    /// The shell the UI detected (it changes when the user runs `zsh` or `pwsh`
    /// inside the tab); before that, the shell that was launched.
    shell: Mutex<String>,
}

/// First shell on PATH from the platform's preference list.
/// Windows: PowerShell 7 → Windows PowerShell → cmd. Unix: `$SHELL` → bash → sh.
pub fn default_shell() -> String {
    #[cfg(windows)]
    let candidates: Vec<String> = vec!["pwsh.exe".into(), "powershell.exe".into(), "cmd.exe".into()];
    #[cfg(not(windows))]
    let candidates: Vec<String> = {
        let mut v = Vec::new();
        if let Ok(s) = std::env::var("SHELL") {
            if !s.is_empty() {
                v.push(s);
            }
        }
        v.extend(["/bin/bash".into(), "/bin/sh".into()]);
        v
    };
    // Always return an absolute path: a bare name (e.g. an odd $SHELL, or no
    // $SHELL at all) would otherwise break spawning on hosts without it on PATH.
    candidates
        .iter()
        .find_map(|c| find_on_path(c))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| candidates.last().cloned().unwrap_or_default())
}

/// Absolute paths must exist; bare names are searched on PATH.
fn find_on_path(prog: &str) -> Option<PathBuf> {
    let p = Path::new(prog);
    if p.is_absolute() || p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(prog)).find(|c| c.is_file())
}

// ---------------------------------------------------------------- elevation ("Local (admin)")

/// How an elevated local shell can be delivered on this machine.
pub enum Elevation {
    /// Run `program args…` inside our PTY (`sudo --inline <shell>` / `sudo -i`).
    Inline { program: String, args: Vec<String> },
    /// Nothing can be embedded: caller opens a separate elevated window and shows `why`.
    NewWindow { why: String },
}

/// Windows sudo modes (`HKLM\…\Sudo\Enabled`): 0 disabled, 1 forceNewWindow, 2 disableInput, 3 normal/inline.
#[cfg(windows)]
const SUDO_INLINE: u32 = 3;

/// Decide how to elevate `shell`.
///
/// Windows: `sudo.exe` (24H2+) in inline mode wraps the shell in our ConPTY;
/// any other mode, or no sudo, means a separate window — an elevated ConPTY
/// cannot be created from a non-elevated process. Unix: `sudo -i` in the PTY;
/// the password prompt comes through like any other output.
pub fn elevation_for(shell: &str) -> Elevation {
    #[cfg(windows)]
    {
        let Some(sudo) = find_on_path("sudo.exe") else {
            return Elevation::NewWindow { why: "Windows sudo (sudo.exe, Windows 11 24H2+) is not available".into() };
        };
        match windows_sudo_mode() {
            Some(SUDO_INLINE) => Elevation::Inline {
                program: sudo.to_string_lossy().into_owned(),
                args: vec!["--inline".into(), shell.to_string()],
            },
            Some(0) | None => Elevation::NewWindow {
                why: "Windows sudo is disabled (Settings › System › For developers › Enable sudo, mode Inline)".into(),
            },
            Some(_) => Elevation::NewWindow {
                why: "Windows sudo is not in Inline mode (Settings › System › For developers › sudo)".into(),
            },
        }
    }
    #[cfg(not(windows))]
    {
        let _ = shell;
        Elevation::Inline { program: "sudo".into(), args: vec!["-i".into()] }
    }
}

/// Effective sudo mode: the user setting, capped by group policy when one is set.
/// Read with `reg query` so no registry crate is needed. `None` = key absent.
#[cfg(windows)]
fn windows_sudo_mode() -> Option<u32> {
    let read = |key: &str| -> Option<u32> {
        let out = hidden_command("reg").args(["query", key, "/v", "Enabled"]).output().ok()?;
        parse_reg_dword(&String::from_utf8_lossy(&out.stdout))
    };
    let setting = read(r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Sudo")?;
    Some(match read(r"HKLM\SOFTWARE\Policies\Microsoft\Windows\Sudo") {
        Some(policy) => setting.min(policy),
        None => setting,
    })
}

/// `reg query` prints `    Enabled    REG_DWORD    0x3`.
#[cfg(windows)]
fn parse_reg_dword(out: &str) -> Option<u32> {
    let line = out.lines().find(|l| l.contains("REG_DWORD"))?;
    let hex = line.split_whitespace().last()?.trim_start_matches("0x");
    u32::from_str_radix(hex, 16).ok()
}

/// A console-less child on Windows (`reg`, `powershell`): no flashing window.
#[cfg(windows)]
fn hidden_command(prog: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = std::process::Command::new(prog);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

/// Fallback: start `shell` elevated in its own console via UAC (ShellExecute "runas").
#[cfg(windows)]
pub fn open_elevated_window(shell: &str) -> Result<()> {
    let ps = format!("Start-Process -FilePath '{}' -Verb RunAs", shell.replace('\'', "''"));
    hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .spawn()
        .context("start elevated window")?;
    Ok(())
}

/// Where PTY output and the exit notice go. The app wires this to Tauri events;
/// tests wire it to a channel.
pub trait Sink: Send + Sync + 'static {
    fn data(&self, bytes: &[u8]);
    fn closed(&self, reason: &str);
}

struct AppSink {
    app: AppHandle,
    id: String,
}

impl Sink for AppSink {
    fn data(&self, bytes: &[u8]) {
        SessionManager::emit_data(&self.app, &self.id, bytes);
    }
    fn closed(&self, reason: &str) {
        SessionManager::emit_closed(&self.app, &self.id, reason);
    }
}

impl LocalSession {
    pub fn spawn(app: AppHandle, id: String, p: LocalParams) -> Result<Arc<Self>> {
        Self::spawn_with(AppSink { app, id: id.clone() }, &id, p)
    }

    pub fn spawn_with(sink: impl Sink, id: &str, p: LocalParams) -> Result<Arc<Self>> {
        let requested = p.shell.filter(|s| !s.trim().is_empty()).unwrap_or_else(default_shell);
        // Resolve to an absolute path (a bare program name from a node config or a
        // bare $SHELL should still launch); `program` is what we run and report.
        let program = find_on_path(&requested).map(|p| p.to_string_lossy().into_owned()).unwrap_or(requested);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: p.rows, cols: p.cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow!("open pty: {e}"))?;

        let mut cmd = CommandBuilder::new(&program);
        for a in &p.args {
            cmd.arg(a);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(cwd) = p.cwd.filter(|c| !c.trim().is_empty()) {
            cmd.cwd(cwd);
        } else if let Some(home) = dirs::home_dir() {
            cmd.cwd(home);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("start {program}: {e}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow!("pty reader: {e}"))?;
        let writer = pair.master.take_writer().map_err(|e| anyhow!("pty writer: {e}"))?;
        let killer = child.clone_killer();

        // Reader thread pumps PTY output until EOF; waiter thread reaps the child.
        // Whichever finishes first reports the close (once), preferring the exit code.
        let sink = Arc::new(sink);
        let closed = Arc::new(AtomicBool::new(false));
        let short = &id[..8.min(id.len())];
        // Emitter thread coalesces reads into ≤ 1 sink.data call per 16 ms (same
        // Coalescer as SSH), so a flood becomes a handful of events. It waits only
        // as long as a buffered tail is due, then flushes even with no more reads —
        // so a paste echo never sits unrendered waiting for the next chunk.
        let (dtx, drx) = std::sync::mpsc::channel::<Ev>();
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let typing = Arc::new(Mutex::new(Typing::default()));
        {
            let sink = sink.clone();
            let (writer, typing) = (writer.clone(), typing.clone());
            let co_id = format!("local:{short}");
            let log_id = short.to_string();
            std::thread::Builder::new()
                .name(format!("pty-emit-{short}"))
                .spawn(move || emit_loop(drx, sink, writer, typing, co_id, log_id))
                .context("spawn pty emitter thread")?;
        }
        {
            let (sink, closed, dtx) = (sink.clone(), closed.clone(), dtx.clone());
            std::thread::Builder::new()
                .name(format!("pty-read-{short}"))
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break, // EOF
                            Ok(n) => { let _ = dtx.send(Ev::Data(buf[..n].to_vec())); }
                            // Retry a signal-interrupted read; drain buffered output first.
                            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                            // On a Unix PTY the master read returns EIO once the child has
                            // exited — that is the normal end of stream, after all buffered
                            // bytes have been delivered by earlier Ok(n) reads.
                            Err(_) => break,
                        }
                    }
                    let _ = dtx.send(Ev::Eof); // let the emitter flush its remainder and exit
                    // Give the waiter a moment to report the real exit code first.
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    if !closed.swap(true, Ordering::SeqCst) {
                        sink.closed("closed");
                    }
                })
                .context("spawn pty reader thread")?;
        }
        {
            std::thread::Builder::new()
                .name(format!("pty-wait-{short}"))
                .spawn(move || {
                    let reason = match child.wait() {
                        Ok(st) => format!("exit {}", st.exit_code()),
                        Err(e) => format!("wait failed: {e}"),
                    };
                    if !closed.swap(true, Ordering::SeqCst) {
                        sink.closed(&reason);
                    }
                })
                .context("spawn pty wait thread")?;
        }

        // `program` is the absolute path; the UI basenames it for the tab title.
        let shell = p.shell_name.unwrap_or_else(|| program.clone());
        Ok(Arc::new(Self {
            master: Mutex::new(pair.master),
            writer,
            killer: Mutex::new(killer),
            program,
            sudo_password: p.sudo_password,
            typing,
            events: Mutex::new(dtx),
            shell: Mutex::new(shell),
        }))
    }
}

fn write_pty(writer: &Mutex<Box<dyn Write + Send>>, data: &[u8]) -> std::io::Result<()> {
    let mut w = writer.lock().unwrap();
    w.write_all(data)?;
    w.flush()
}

/// The emitter thread: coalesce output to the sink, and run hidden commands
/// through `hidden::Capture` (the same state machine as SSH) so neither the typed
/// line nor its output nor the redrawn prompt reaches the terminal.
fn emit_loop(
    rx: std::sync::mpsc::Receiver<Ev>,
    sink: Arc<impl Sink>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    typing: Arc<Mutex<Typing>>,
    co_id: String,
    id: String,
) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;
    let mut co = {
        let sink = sink.clone();
        crate::session::Coalescer::new(co_id, move |b: &[u8]| sink.data(b))
    };
    let mut capture: Option<(crate::hidden::Capture, Reply)> = None;
    // A hidden command waiting to be typed: the shell still expects the terminal's
    // answer to a cursor query (PSReadLine, right after the prompt), and would read
    // the command as that answer. Typed once answered, or after REPORT_WAIT anyway.
    let mut queued: Option<(Vec<u8>, Option<(String, String)>, Reply, std::time::Instant)> = None;
    const REPORT_WAIT: Duration = Duration::from_secs(1);
    // End a capture: answer the caller, then type what the user typed meanwhile.
    let finish = |capture: &mut Option<(crate::hidden::Capture, Reply)>, r: Result<(), String>| {
        if let Some((cap, reply)) = capture.take() {
            let _ = reply.send(r.map(|_| cap.into_result()).map_err(|e| anyhow!(e)));
        }
        let mut t = typing.lock().unwrap();
        t.capturing = false;
        let queued = std::mem::take(&mut t.pending);
        if !queued.is_empty() {
            let _ = write_pty(&writer, &queued);
        }
    };
    loop {
        // Block until output arrives, the coalesced tail is due, or (capturing)
        // the next 100 ms tick for the quiet-tail end and the timeout.
        let mut wait = co.due_in().unwrap_or(Duration::from_secs(3600));
        if capture.is_some() {
            wait = wait.min(Duration::from_millis(100));
        }
        if queued.is_some() {
            wait = wait.min(Duration::from_millis(20));
        }
        match rx.recv_timeout(wait) {
            Ok(Ev::Data(chunk)) => {
                typing.lock().unwrap().prompt.output(&chunk);
                match capture.as_mut() {
                    Some((cap, _)) => {
                        let show = cap.feed(&chunk);
                        if !show.is_empty() {
                            co.flush_close(); // keep ordering behind earlier output
                            sink.data(&show);
                        }
                        // Answer the cursor queries the capture swallowed (PSReadLine waits for them).
                        if let Some((r, n)) = cap.take_replies() {
                            typing.lock().unwrap().prompt.answered(n);
                            let _ = write_pty(&writer, &r);
                        }
                        // The login user's own sudo prompt, when MCP allowed it. Never logged.
                        if let Some(pw) = cap.take_password_send() {
                            tracing::info!(session = %id, "sudo: prompt detected, sent=true");
                            let _ = write_pty(&writer, &pw);
                        }
                        if cap.done() {
                            finish(&mut capture, Ok(()));
                        }
                    }
                    None => co.push(&chunk),
                }
            }
            Ok(Ev::Capture { line, sudo, reply }) => {
                if capture.is_some() || queued.is_some() {
                    let _ = reply.send(Err(anyhow!("another hidden command is still running")));
                } else {
                    queued = Some((line, sudo, reply, std::time::Instant::now()));
                }
            }
            Ok(Ev::Eof) | Err(RecvTimeoutError::Disconnected) => {
                co.flush_close();
                if let Some((_, _, reply, _)) = queued.take() {
                    let _ = reply.send(Err(anyhow!("session closed")));
                }
                finish(&mut capture, Err("session closed".into()));
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                if co.due_in().is_some_and(|d| d.is_zero()) {
                    co.flush_timer();
                }
                if let Some(r) = capture.as_mut().and_then(|(c, _)| c.tick()) {
                    finish(&mut capture, r);
                }
            }
        }
        // Type the queued hidden command once the shell isn't waiting on the terminal.
        if capture.is_none() {
            if let Some((line, sudo, reply, since)) = queued.take() {
                let mut t = typing.lock().unwrap();
                if t.prompt.awaiting_report() && since.elapsed() < REPORT_WAIT {
                    queued = Some((line, sudo, reply, since));
                    continue;
                }
                co.flush_close(); // don't let buffered output land inside the capture window
                if let Err(why) = t.prompt.ready() {
                    tracing::info!(session = %id, "hidden command refused: {why}");
                    let _ = reply.send(Err(anyhow!("{why}")));
                } else if let Err(e) = write_pty(&writer, &line) {
                    let _ = reply.send(Err(anyhow!("write to the shell: {e}")));
                } else {
                    t.capturing = true;
                    let mut cap = crate::hidden::Capture::new();
                    if let Some((pw, user)) = sudo {
                        cap.set_sudo_password(pw, user);
                    }
                    capture = Some((cap, reply));
                }
            }
        }
    }
}

/// This machine's name, for MCP `list_sessions` (`host` of a local session).
pub fn hostname() -> String {
    let from_env = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok();
    let from_file = || ["/proc/sys/kernel/hostname", "/etc/hostname"].iter().find_map(|f| std::fs::read_to_string(f).ok());
    let from_uname = || std::process::Command::new("uname").arg("-n").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    from_env
        .or_else(from_file)
        .or_else(from_uname)
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

fn basename(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_string()
}

/// The OS account this app runs as (the user a local `sudo` prompt names).
fn os_user() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default()
}

#[async_trait]
impl Session for LocalSession {
    async fn write(&self, data: &[u8]) -> Result<()> {
        // While a hidden command captures, hold keystrokes until it is done: typed
        // into the middle of it they would end up in its output or its command line.
        let mut t = self.typing.lock().unwrap();
        t.prompt.input(data);
        if t.capturing {
            t.pending.extend_from_slice(data);
            return Ok(());
        }
        write_pty(&self.writer, data)?;
        Ok(())
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow!("resize pty: {e}"))
    }

    async fn close(&self) -> Result<()> {
        let _ = self.killer.lock().unwrap().kill();
        Ok(())
    }

    async fn fs(&self) -> Result<Arc<dyn crate::sftp::Fs>> {
        Ok(Arc::new(crate::sftp::LocalFs))
    }

    async fn exec_hidden(&self, cmd: &str) -> Result<(Vec<u8>, i32)> {
        self.exec_hidden_sudo(cmd, false).await
    }

    /// Type `cmd` into the shell wrapped in sentinels and capture its output, out
    /// of the user's sight (as SSH does). bash/zsh and PowerShell; `allow_sudo`
    /// (MCP, tier-gated) lets it answer the OS user's own sudo prompt with the
    /// stored password on Unix.
    async fn exec_hidden_sudo(&self, cmd: &str, allow_sudo: bool) -> Result<(Vec<u8>, i32)> {
        let shell = self.shell.lock().unwrap().clone();
        let dialect = crate::hidden::Dialect::of(&shell).ok_or_else(|| {
            anyhow!("hidden commands need bash, zsh or PowerShell; this tab is running {}", basename(&shell))
        })?;
        // UAC shows on the secure desktop: nothing can answer it for the agent, and
        // the command would hang until the timeout.
        #[cfg(windows)]
        if let Some(what) = crate::hidden::uac_trigger(cmd) {
            return Err(anyhow!(
                "refused: {what} would open a UAC prompt that cannot be answered from here; run it yourself in a Local (admin) tab"
            ));
        }
        #[cfg(unix)]
        let sudo = if allow_sudo { self.sudo_password.clone().map(|pw| (pw, os_user())) } else { None };
        #[cfg(not(unix))]
        let sudo = {
            let _ = (allow_sudo, os_user);
            None
        };
        let (reply, rx) = oneshot::channel();
        self.events
            .lock()
            .unwrap()
            .send(Ev::Capture { line: dialect.wrap(cmd).into_bytes(), sudo, reply })
            .map_err(|_| anyhow!("session closed"))?;
        rx.await.map_err(|_| anyhow!("session closed"))?
    }

    fn shell_changed(&self, shell: Option<&str>) {
        if let Some(s) = shell.filter(|s| !s.is_empty()) {
            *self.shell.lock().unwrap() = s.to_string();
        }
    }

    /// Password offered (never shown) to a sudo/su prompt in this shell. Unix only:
    /// Windows elevation is UAC, which takes no tty password, so a local Windows
    /// shell exposes none (the UI also hides the button there).
    async fn ssh_password(&self) -> Option<String> {
        #[cfg(unix)]
        {
            self.sudo_password.clone()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_is_something_that_exists() {
        let s = default_shell();
        assert!(find_on_path(&s).is_some(), "{s} not found");
        #[cfg(windows)]
        assert!(s.ends_with(".exe"));
    }

    struct ChanSink(Mutex<std::sync::mpsc::Sender<Result<Vec<u8>, String>>>);
    impl Sink for ChanSink {
        fn data(&self, b: &[u8]) { let _ = self.0.lock().unwrap().send(Ok(b.to_vec())); }
        fn closed(&self, r: &str) { let _ = self.0.lock().unwrap().send(Err(r.to_string())); }
    }

    #[test]
    fn spawns_a_real_pty_and_reports_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        #[cfg(windows)]
        let (shell, args) = ("cmd.exe", vec!["/c".to_string(), "echo charon-pty-ok".to_string()]);
        #[cfg(not(windows))]
        let (shell, args) = ("/bin/sh", vec!["-c".to_string(), "echo charon-pty-ok".to_string()]);
        let p = LocalParams { shell: Some(shell.into()), args, cwd: None, cols: 80, rows: 24, sudo_password: None, shell_name: None };
        let sess = LocalSession::spawn_with(ChanSink(Mutex::new(tx)), "test-session", p).unwrap();
        // The resolver turns the shell into an absolute path (even a bare name),
        // so the session reports that resolved path.
        let expected = find_on_path(shell).unwrap().to_string_lossy().into_owned();
        assert_eq!(sess.program, expected);
        assert!(Path::new(&sess.program).is_absolute());

        // The reader and waiter threads race, so the exit notice can arrive before
        // the buffered output. Wait for the exit event, then keep draining until a
        // short quiet gap so trailing output is collected. (Don't wait for the
        // channel to fully disconnect: the reader can stay blocked in read() until
        // the master is dropped, which outlives this session.)
        use std::sync::mpsc::RecvTimeoutError;
        let mut out = Vec::new();
        let mut reason = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Ok(b)) => out.extend(b),
                Ok(Err(r)) => reason = Some(r),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if reason.is_some() {
                        break; // exited and 500 ms with no more output → drained
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("charon-pty-ok"), "output: {text:?}");
        assert_eq!(reason.as_deref(), Some("exit 0"));
    }

    /// Collects everything the terminal would be shown, and why the shell ended.
    struct Screen(Arc<Mutex<Vec<u8>>>, Arc<Mutex<Option<String>>>);
    impl Sink for Screen {
        fn data(&self, b: &[u8]) { self.0.lock().unwrap().extend_from_slice(b); }
        fn closed(&self, r: &str) { *self.1.lock().unwrap() = Some(r.to_string()); }
    }

    /// The end of the screen and the close reason, for a failure message.
    fn dump(screen: &Mutex<Vec<u8>>, closed: &Mutex<Option<String>>) -> String {
        let s = screen.lock().unwrap();
        let tail = String::from_utf8_lossy(&s[s.len().saturating_sub(2000)..]).into_owned();
        format!("closed={:?}\nscreen tail: {tail:?}", closed.lock().unwrap())
    }

    /// Wait until the screen (from byte `from` on) contains `needle`.
    fn wait_for(screen: &Mutex<Vec<u8>>, needle: &[u8], from: usize, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            {
                let s = screen.lock().unwrap();
                if s.len() > from && s[from..].windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// Be the terminal: answer each cursor-position query that reaches the screen
    /// (PSReadLine on Unix asks at every prompt and waits), through `write` as
    /// xterm's reply would come. Stops when the session is dropped.
    fn answer_cursor_queries(sess: &Arc<LocalSession>, screen: &Arc<Mutex<Vec<u8>>>) {
        let (sess, screen) = (Arc::downgrade(sess), screen.clone());
        std::thread::spawn(move || {
            let mut answered = 0;
            while let Some(s) = sess.upgrade() {
                let asked = screen.lock().unwrap().windows(4).filter(|w| *w == b"\x1b[6n").count();
                for _ in answered..asked {
                    let _ = futures::executor::block_on(s.write(b"\x1b[1;1R"));
                }
                answered = asked;
                drop(s);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    }

    /// Start `program args` in a real PTY and wait for the hook's first prompt (133;B).
    fn hooked_shell(program: &Path, args: Vec<String>) -> (Arc<LocalSession>, Arc<Mutex<Vec<u8>>>, Arc<Mutex<Option<String>>>) {
        let screen = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(None));
        let p = LocalParams {
            shell: Some(program.to_string_lossy().into_owned()), args, cwd: None, cols: 120, rows: 30,
            sudo_password: None, shell_name: None,
        };
        let sess = LocalSession::spawn_with(Screen(screen.clone(), closed.clone()), "hidden-test", p).unwrap();
        answer_cursor_queries(&sess, &screen);
        let ok = wait_for(&screen, b"\x1b]133;B", 0, 60);
        assert!(ok, "no hooked prompt\n{}", dump(&screen, &closed));
        (sess, screen, closed)
    }

    /// Run a hidden command; check nothing of it (typed line, output, sentinels) reached the screen.
    async fn hidden(sess: &LocalSession, screen: &Mutex<Vec<u8>>, closed: &Mutex<Option<String>>, cmd: &str, marker: &str) -> (String, i32) {
        let before = screen.lock().unwrap().len();
        let (out, code) = match tokio::time::timeout(std::time::Duration::from_secs(60), sess.exec_hidden(cmd)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => panic!("hidden command {cmd:?} failed: {e:#}\n{}", dump(screen, closed)),
            Err(_) => panic!("hidden command {cmd:?} timed out\n{}", dump(screen, closed)),
        };
        std::thread::sleep(std::time::Duration::from_millis(400)); // anything that leaks would be here by now
        let shown = String::from_utf8_lossy(&screen.lock().unwrap()[before..]).into_owned();
        assert!(!shown.contains(marker) && !shown.contains("7771"), "leaked to the screen: {shown:?}");
        (crate::hidden::strip_escapes(&String::from_utf8_lossy(&out)), code)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hidden_command_over_a_real_pty_bash() {
        let bash = find_on_path("bash").expect("bash");
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join("rc");
        let hook = include_str!("../../shell-integration/bash.sh");
        std::fs::write(&rc, format!("HISTFILE=/dev/null\nPS1='$ '\n{hook}\n")).unwrap();
        let args = vec!["--noprofile".into(), "--rcfile".into(), rc.to_string_lossy().into_owned(), "-i".into()];
        let (sess, screen, closed) = hooked_shell(&bash, args);

        let (out, code) = hidden(&sess, &screen, &closed, "echo charon-hidden; (exit 3)", "charon-hidden").await;
        assert_eq!((out.trim(), code), ("charon-hidden", 3));
        // The shell is back at its prompt: the next one works too.
        let (out, code) = hidden(&sess, &screen, &closed, "echo again-$((40+2))", "again-").await;
        assert_eq!((out.trim(), code), ("again-42", 0));
        sess.close().await.unwrap();
    }

    #[tokio::test]
    async fn hidden_command_over_a_real_pty_pwsh() {
        let Some(pwsh) = ["pwsh", "pwsh.exe", "powershell.exe"].iter().find_map(|p| find_on_path(p)) else {
            #[cfg(windows)]
            panic!("no PowerShell on Windows");
            #[cfg(not(windows))]
            {
                eprintln!("skipped: no pwsh on PATH");
                return;
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let hook = dir.path().join("hook.ps1");
        std::fs::write(&hook, include_str!("../../shell-integration/pwsh.ps1")).unwrap();
        // CHARON_HOOKED is exported: run from inside a Charon tab, it would skip the hook.
        let setup = format!("Remove-Item Env:CHARON_HOOKED -ErrorAction Ignore; . '{}'", hook.to_string_lossy().replace('\'', "''"));
        let args = vec!["-NoLogo".into(), "-NoProfile".into(), "-NoExit".into(), "-Command".into(), setup];
        let (sess, screen, closed) = hooked_shell(&pwsh, args);

        // A native command's exit code comes through $LASTEXITCODE.
        #[cfg(windows)]
        let native3 = "Write-Output charon-hidden; cmd /c exit 3";
        #[cfg(not(windows))]
        let native3 = "Write-Output charon-hidden; sh -c 'exit 3'";
        let (out, code) = hidden(&sess, &screen, &closed, native3, "charon-hidden").await;
        assert_eq!((out.trim(), code), ("charon-hidden", 3));
        // Success; a cmdlet error ($? false, no native code); a terminating error.
        let (out, code) = hidden(&sess, &screen, &closed, "Write-Output fine", "fine").await;
        assert_eq!((out.trim(), code), ("fine", 0));
        assert_eq!(hidden(&sess, &screen, &closed, "Get-Item ./charon-missing-xyz", "charon-missing").await.1, 1);
        assert_eq!(hidden(&sess, &screen, &closed, "throw 'charon-boom'", "charon-boom").await.1, 1);
        // An earlier native failure doesn't leak into the next command's code.
        assert_eq!(hidden(&sess, &screen, &closed, "Write-Output after", "after").await.1, 0);
        sess.close().await.unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn reg_query_output_parses() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Sudo\r\n    Enabled    REG_DWORD    0x3\r\n\r\n";
        assert_eq!(parse_reg_dword(out), Some(3));
        assert_eq!(parse_reg_dword("ERROR: The system was unable to find the specified registry key or value."), None);
        assert_eq!(parse_reg_dword("    Enabled    REG_DWORD    0x0"), Some(0));
    }

    #[test]
    fn elevation_is_inline_or_explains_why() {
        match elevation_for("cmd.exe") {
            Elevation::Inline { program, args } => {
                assert!(!program.is_empty());
                assert!(args.contains(&"--inline".to_string()) || args == vec!["-i".to_string()]);
            }
            Elevation::NewWindow { why } => assert!(why.contains("sudo"), "{why}"),
        }
    }

    #[test]
    fn find_on_path_handles_absolute_and_bare_names() {
        assert!(find_on_path("definitely-not-a-real-program-xyz").is_none());
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("prog");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(find_on_path(f.to_str().unwrap()), Some(f.clone()));
        assert!(find_on_path(dir.path().join("missing").to_str().unwrap()).is_none());
    }
}
