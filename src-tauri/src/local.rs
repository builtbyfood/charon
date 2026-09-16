//! Local shell in a PTY via portable-pty (ConPTY on Windows, openpty elsewhere).
//! Blocking reads/waits run on plain threads; the `Session` methods are cheap.

use crate::session::{Session, SessionManager};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

pub struct LocalParams {
    /// Program to run; `None` picks the platform default (see `default_shell`).
    pub shell: Option<String>,
    /// Extra arguments (tests use `sh -c` / `cmd /c`).
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

pub struct LocalSession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Basename of what was launched, for the tab title.
    pub program: String,
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
    candidates
        .iter()
        .find(|c| find_on_path(c).is_some())
        .cloned()
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
        let program = p.shell.filter(|s| !s.trim().is_empty()).unwrap_or_else(default_shell);
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
        {
            let (sink, closed) = (sink.clone(), closed.clone());
            std::thread::Builder::new()
                .name(format!("pty-read-{short}"))
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => sink.data(&buf[..n]),
                        }
                    }
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

        let program = Path::new(&program)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(program);
        Ok(Arc::new(Self {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            program,
        }))
    }
}

#[async_trait]
impl Session for LocalSession {
    async fn write(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)?;
        w.flush()?;
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
        let p = LocalParams { shell: Some(shell.into()), args, cwd: None, cols: 80, rows: 24 };
        let sess = LocalSession::spawn_with(ChanSink(Mutex::new(tx)), "test-session", p).unwrap();
        assert_eq!(sess.program, shell);

        let mut out = Vec::new();
        let mut reason = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while reason.is_none() && std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(Ok(b)) => out.extend(b),
                Ok(Err(r)) => reason = Some(r),
                Err(_) => {}
            }
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("charon-pty-ok"), "output: {text:?}");
        assert_eq!(reason.as_deref(), Some("exit 0"));
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
