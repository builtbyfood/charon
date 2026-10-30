//! SSH shell session on top of russh. Password and key auth; host keys are
//! checked against known_hosts (see `hostkeys.rs`) with a trust-on-first-use
//! prompt routed through the UI. Jump hosts: each hop is a full SSH session
//! and the next hop rides a direct-tcpip channel through it (like ProxyJump).

use crate::hostkeys::{fingerprint, HostKeys, Verdict};
use crate::session::{Session, SessionManager};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use russh::client::{self, Handle};
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{mpsc, oneshot, Mutex};

/// How long the user gets to answer the host key prompt before we give up.
const HOSTKEY_PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub struct SshParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    /// Bastions to go through, first hop first.
    pub jumps: Vec<Hop>,
    pub cols: u16,
    pub rows: u16,
    /// Server-alive interval; `None` (profile 0) disables keepalive. Applies to
    /// the whole connection â the shell, SFTP, and sudo channels share it.
    pub keepalive: Option<std::time::Duration>,
    /// Start this shell instead of the account's login shell (`shell_command`).
    pub shell: Option<String>,
}

/// Unix shells that take `-l` (start as a login shell, so profiles load).
const LOGIN_SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "ksh", "mksh", "dash", "ash", "tcsh", "csh", "yash", "oksh"];

fn shell_base(s: &str) -> String {
    let b = s.rsplit(['/', '\\']).next().unwrap_or(s).to_ascii_lowercase();
    b.strip_suffix(".exe").map(str::to_string).unwrap_or(b)
}

/// The exec request that starts a chosen shell on the pty. A Unix shell by name or
/// path runs as a login shell; anything else (pwsh, cmd, or a command with its own
/// arguments) runs as written.
pub fn shell_command(shell: &str) -> String {
    let s = shell.trim();
    if !s.contains(char::is_whitespace) && LOGIN_SHELLS.contains(&shell_base(s).as_str()) {
        format!("{s} -l")
    } else {
        s.to_string()
    }
}

/// Shells a host offers. `etc_shells` = `/etc/shells` (Unix): its paths, one per
/// shell name (first path wins), without restricted/non-shell entries. Otherwise
/// `where` output from Windows: the shell names found.
pub fn parse_shells(out: &str, etc_shells: bool) -> Vec<String> {
    const SKIP: &[&str] = &["nologin", "false", "git-shell", "rbash", "screen", "tmux", "sync", "true"];
    let mut seen = std::collections::HashSet::new();
    let mut shells = Vec::new();
    for line in out.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if etc_shells && !line.starts_with('/') {
            continue;
        }
        let base = shell_base(line);
        if SKIP.contains(&base.as_str()) || !seen.insert(base.clone()) {
            continue;
        }
        shells.push(if etc_shells { line.to_string() } else { base });
    }
    shells
}

/// One bastion on the way to the target.
pub struct Hop {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
}

/// How to authenticate. Key material is tried first when both are present.
#[derive(Clone)]
pub enum Auth {
    Password(String),
    /// `key` is a path to a private key file or the PEM text itself.
    Key { key: String, passphrase: Option<String> },
}

impl Auth {
    fn load_key(key: &str, passphrase: Option<&str>) -> Result<russh_keys::key::KeyPair> {
        // Key text (OpenSSH/PEM or PuTTY) has a recognisable first line; anything else is a path.
        let text = if key.trim_start().starts_with("-----") || crate::ppk::is_ppk(key) {
            key.to_string()
        } else if Path::new(key).is_file() {
            std::fs::read_to_string(key).with_context(|| format!("read private key {key}"))?
        } else {
            return Err(anyhow!("private key file not found: {key}"));
        };
        if crate::ppk::is_ppk(&text) {
            crate::ppk::load(&text, passphrase).with_context(|| format!("load PuTTY key {key}"))
        } else {
            russh_keys::decode_secret_key(&text, passphrase).with_context(|| format!("decode private key {key}"))
        }
    }
}

/// Payload of the `ssh:hostkey:{session}` event.
#[derive(Debug, Clone, Serialize)]
pub struct HostKeyPrompt {
    /// Session id to answer with (`ssh_hostkey_answer`).
    pub session: String,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    /// Where an accepted key will be written.
    pub known_hosts: String,
}

/// Details of a host key mismatch, handed to the UI so it can offer the right fix.
#[derive(Debug, Clone, Serialize)]
pub struct HostKeyChanged {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    /// File holding the stale entry and its 1-based line.
    pub file: String,
    pub line: usize,
    /// True when `file` is Charon's own known_hosts (we can remove the line);
    /// false for the user's ~/.ssh/known_hosts (read-only: tell them the command).
    pub own: bool,
}

impl std::fmt::Display for HostKeyChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HOST KEY CHANGED for {}:{} â the {} key the server presented ({}) does not match \
             the one recorded in {} line {}. Someone could be intercepting this connection.",
            self.host, self.port, self.key_type, self.fingerprint, self.file, self.line
        )
    }
}

/// Handler errors. russh requires `From<russh::Error>`; the extra variants carry
/// the host key outcome so the UI gets a sentence, not an error code.
#[derive(Debug)]
pub enum SshError {
    Russh(russh::Error),
    HostKeyChanged(HostKeyChanged),
    HostKeyRejected,
    HostKeyTimeout,
}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        Self::Russh(e)
    }
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Russh(e) => write!(f, "{e}"),
            Self::HostKeyChanged(d) => write!(f, "{d}"),
            Self::HostKeyRejected => f.write_str("host key rejected"),
            Self::HostKeyTimeout => write!(f, "no answer to the host key prompt within {HOSTKEY_PROMPT_TIMEOUT:?}; connection dropped"),
        }
    }
}

impl std::error::Error for SshError {}

struct Handler {
    mgr: SessionManager,
    id: String,
    host: String,
    port: u16,
    keys: Arc<HostKeys>,
}

#[async_trait]
impl client::Handler for Handler {
    type Error = SshError;

    async fn check_server_key(&mut self, key: &russh_keys::key::PublicKey) -> Result<bool, Self::Error> {
        let verdict = self.keys.check(&self.host, self.port, key);
        tracing::info!(session = %self.id, host = %self.host, port = self.port, key_type = key.name(),
            fingerprint = %fingerprint(key), ?verdict, "host key check");
        match verdict {
            Verdict::Known => Ok(true),
            Verdict::Changed { file, line } => Err(SshError::HostKeyChanged(HostKeyChanged {
                host: self.host.clone(),
                port: self.port,
                key_type: key.name().to_string(),
                fingerprint: fingerprint(key),
                own: self.keys.is_own(&file),
                file: file.display().to_string(),
                line,
            })),
            Verdict::Unknown => {
                // Ask the UI and wait. Registering the channel before emitting means an
                // instant answer cannot race the registration.
                let rx = self.mgr.expect_hostkey_answer(&self.id).await;
                let prompt = HostKeyPrompt {
                    session: self.id.clone(),
                    host: self.host.clone(),
                    port: self.port,
                    key_type: key.name().to_string(),
                    fingerprint: fingerprint(key),
                    known_hosts: self.keys.own_path().display().to_string(),
                };
                let event = format!("ssh:hostkey:{}", self.id);
                let sent = self.mgr.app().emit(&event, prompt);
                crate::session::wake_ui();
                match sent {
                    Ok(()) => tracing::info!(session = %self.id, %event, "host key prompt emitted; waiting for answer"),
                    Err(e) => tracing::error!(session = %self.id, %event, "could not emit host key prompt: {e}"),
                }
                let answer = tokio::time::timeout(HOSTKEY_PROMPT_TIMEOUT, rx).await;
                self.mgr.cancel_hostkey_prompt(&self.id).await;
                match answer {
                    Ok(Ok(true)) => {
                        match self.keys.learn(&self.host, self.port, key) {
                            Ok(()) => tracing::info!(session = %self.id, file = %self.keys.own_path().display(), "host key accepted and recorded"),
                            Err(e) => tracing::warn!(session = %self.id, "host key accepted but not recorded: {e:#}"),
                        }
                        Ok(true)
                    }
                    Ok(Ok(false)) => {
                        tracing::info!(session = %self.id, "host key rejected by user");
                        Err(SshError::HostKeyRejected)
                    }
                    Ok(Err(_)) => {
                        tracing::warn!(session = %self.id, "host key prompt dropped without an answer");
                        Err(SshError::HostKeyRejected)
                    }
                    Err(_) => {
                        tracing::warn!(session = %self.id, "no host key answer within {HOSTKEY_PROMPT_TIMEOUT:?}");
                        Err(SshError::HostKeyTimeout)
                    }
                }
            }
        }
    }
}

/// Commands sent to the task that owns the channel.
enum Cmd {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
    /// Type `line`, capture between sentinels, hide the whole exchange from the UI.
    /// `allow_sudo` lets the capture auto-answer the login user's sudo prompt.
    Capture { line: Vec<u8>, allow_sudo: bool, reply: oneshot::Sender<Result<(Vec<u8>, i32)>> },
}

pub struct SshSession {
    handle: Mutex<Handle<Handler>>,
    /// Bastion sessions, kept alive for as long as the target session lives.
    jumps: Mutex<Vec<Handle<Handler>>>,
    tx: mpsc::Sender<Cmd>,
    /// SFTP subsystem on the same connection, opened on first use.
    sftp: Mutex<Option<Arc<crate::sftp::SftpFs>>>,
    /// The password used for SSH (password auth only), reused for sudo -S.
    auth_password: Option<String>,
}

/// Authenticate an open handle; the error names the host so a failed hop is obvious.
async fn authenticate(handle: &mut Handle<Handler>, user: &str, host: &str, auth: &Auth) -> Result<()> {
    let authed = match auth {
        Auth::Key { key, passphrase } => {
            let key = Auth::load_key(key, passphrase.as_deref())?;
            handle.authenticate_publickey(user, Arc::new(key)).await?
        }
        Auth::Password(pw) => {
            // Try the plain "password" method first, then fall back to keyboard-
            // interactive with the same password. Many servers (HPE iLO, and any
            // OpenSSH with `PasswordAuthentication no` + `KbdInteractiveAuthentication
            // yes`) accept a password *only* through keyboard-interactive, which they
            // often present as an "MFA"/challenge — so a plain password attempt is
            // rejected and it looks like MFA is required when it isn't.
            if handle.authenticate_password(user, pw).await? {
                true
            } else {
                keyboard_interactive(handle, user, pw).await?
            }
        }
    };
    if !authed {
        return Err(anyhow!("authentication rejected for {user}@{host}"));
    }
    Ok(())
}

/// Answer keyboard-interactive prompts with the password. This covers the common
/// single-factor case (one non-echo "Password:" prompt); a genuine second factor
/// (an OTP prompt we can't fill from a stored password) is left to fail cleanly
/// rather than loop. Echoed prompts get an empty answer so the password is never
/// sent to a visible field.
async fn keyboard_interactive(handle: &mut Handle<Handler>, user: &str, pw: &str) -> Result<bool> {
    use russh::client::KeyboardInteractiveAuthResponse as Kir;
    let mut resp = handle.authenticate_keyboard_interactive_start(user, None).await?;
    for _ in 0..10 {
        match resp {
            Kir::Success => return Ok(true),
            Kir::Failure => return Ok(false),
            Kir::InfoRequest { prompts, .. } => {
                let answers = prompts.iter().map(|p| if p.echo { String::new() } else { pw.to_string() }).collect();
                resp = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
    Ok(false)
}

impl SshSession {
    pub async fn connect(mgr: SessionManager, keys: Arc<HostKeys>, id: String, p: SshParams) -> Result<Arc<Self>> {
        // No inactivity timeout (it dropped idle sessions); keep the link alive and
        // detect dead ones with server-alive requests instead. keepalive_max=3 means
        // ~3 missed replies (â 3Ã interval) before russh gives up.
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: p.keepalive,
            keepalive_max: 3,
            ..Default::default()
        });

        let app = mgr.app().clone();
        let handler = |host: &str, port: u16| Handler {
            mgr: mgr.clone(),
            id: id.clone(),
            host: host.to_string(),
            port,
            keys: keys.clone(),
        };

        // Walk the bastions. Each hop is authenticated in turn, then asked to open a
        // direct-tcpip channel to the next host; that channel is the next hop's TCP.
        let mut jumps: Vec<Handle<Handler>> = Vec::new();
        let mut tunnel: Option<russh::ChannelStream<client::Msg>> = None;
        for (i, hop) in p.jumps.iter().enumerate() {
            let next = p.jumps.get(i + 1).map(|h| (h.host.as_str(), h.port)).unwrap_or((p.host.as_str(), p.port));
            tracing::info!(session = %id, hop = %hop.host, port = hop.port, next = %next.0, "connecting via jump host");
            let mut h = match tunnel.take() {
                None => client::connect(config.clone(), (hop.host.as_str(), hop.port), handler(&hop.host, hop.port)).await,
                Some(stream) => client::connect_stream(config.clone(), stream, handler(&hop.host, hop.port)).await,
            }
            .with_context(|| format!("connect jump host {}:{}", hop.host, hop.port))?;
            authenticate(&mut h, &hop.user, &hop.host, &hop.auth).await?;
            let chan = h
                .channel_open_direct_tcpip(next.0, next.1 as u32, "127.0.0.1", 0)
                .await
                .with_context(|| format!("{} could not reach {}:{}", hop.host, next.0, next.1))?;
            tunnel = Some(chan.into_stream());
            jumps.push(h);
        }

        let mut handle = match tunnel.take() {
            None => client::connect(config, (p.host.as_str(), p.port), handler(&p.host, p.port)).await,
            Some(stream) => client::connect_stream(config, stream, handler(&p.host, p.port)).await,
        }
        .with_context(|| format!("connect {}:{}", p.host, p.port))?;
        authenticate(&mut handle, &p.user, &p.host, &p.auth).await?;

        let mut channel = handle.channel_open_session().await?;
        channel
            .request_pty(false, "xterm-256color", p.cols as u32, p.rows as u32, 0, 0, &[])
            .await?;
        match p.shell.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(shell) => {
                let cmd = shell_command(shell);
                tracing::info!(session = %id, shell = %cmd, "starting chosen shell");
                channel.exec(true, cmd).await?;
            }
            None => channel.request_shell(true).await?,
        }

        let (tx, mut rx) = mpsc::channel::<Cmd>(256);
        // Password (password-auth only) used to auto-answer the login user's own
        // sudo prompt during a hidden command (MCP run_command, tier-gated per call).
        // Never logged.
        let auth_pw = match &p.auth {
            Auth::Password(pw) => Some(pw.clone()),
            _ => None,
        };
        let login_user = p.user.clone();

        // One task owns the channel: pumps output to the UI and applies queued commands.
        // While a hidden command runs, output goes through `Capture` first.
        tokio::spawn(async move {
            // Coalesce output into ≤ 1 UI event per 16 ms (see session::Coalescer).
            let mut out = crate::session::Coalescer::new(id.clone(), {
                let (app, id) = (app.clone(), id.clone());
                move |b: &[u8]| SessionManager::emit_data(&app, &id, b)
            });
            let mut capture: Option<(crate::hidden::Capture, oneshot::Sender<Result<(Vec<u8>, i32)>>)> = None;
            // User keystrokes / pastes that arrive while a hidden command is capturing
            // are held here and flushed once it finishes, so they never interleave with
            // (or get dropped by) the capture window.
            let mut pending_writes: Vec<u8> = Vec::new();
            // Hidden commands are typed into the shell: only at an empty prompt.
            let mut prompt = crate::hidden::PromptState::default();
            let finish = |c: Option<(crate::hidden::Capture, oneshot::Sender<Result<(Vec<u8>, i32)>>)>, r: Result<(), String>| {
                if let Some((cap, reply)) = c {
                    let _ = reply.send(r.map(|_| cap.into_result()).map_err(|e| anyhow!(e)));
                }
            };
            loop {
                let flush_due = out.due_in();
                tokio::select! {
                    msg = channel.wait() => match msg {
                        Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                            prompt.output(&data);
                            match capture.as_mut() {
                                Some((cap, _)) => {
                                    let show = cap.feed(&data);
                                    // Keep ordering: drain any coalesced normal output first.
                                    if !show.is_empty() { out.flush_close(); SessionManager::emit_data(&app, &id, &show); }
                                    // Auto-answer a sudo/su prompt so hidden commands don't hang.
                                    let pw_send = cap.take_password_send();
                                    if cap.done() { finish(capture.take(), Ok(())); }
                                    if let Some(bytes) = pw_send {
                                        tracing::info!(session = %id, "sudo: prompt detected, sent=true");
                                        let _ = channel.data(&bytes[..]).await;
                                    }
                                }
                                None => out.push(&data),
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            out.flush_close();
                            SessionManager::emit_closed(&app, &id, &format!("exit {exit_status}"));
                        }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                            out.flush_close();
                            SessionManager::emit_closed(&app, &id, "closed");
                            break;
                        }
                        _ => {}
                    },
                    cmd = rx.recv() => match cmd {
                        // Queue input while a hidden command captures; flush after (below).
                        Some(Cmd::Write(b)) => {
                            prompt.input(&b);
                            if capture.is_some() { pending_writes.extend_from_slice(&b); }
                            else { let _ = channel.data(&b[..]).await; }
                        }
                        Some(Cmd::Resize(c, r)) => { let _ = channel.window_change(c as u32, r as u32, 0, 0).await; }
                        Some(Cmd::Capture { line, allow_sudo, reply }) => {
                            out.flush_close(); // don't let buffered output land inside the capture window
                            if capture.is_some() {
                                let _ = reply.send(Err(anyhow!("another hidden command is still running")));
                            } else if let Err(why) = prompt.ready() {
                                tracing::info!(session = %id, "hidden command refused: {why}");
                                let _ = reply.send(Err(anyhow!("{why}")));
                            } else if channel.data(&line[..]).await.is_err() {
                                let _ = reply.send(Err(anyhow!("channel gone")));
                            } else {
                                let mut cap = crate::hidden::Capture::new();
                                if allow_sudo {
                                    if let Some(pw) = &auth_pw { cap.set_sudo_password(pw.clone(), login_user.clone()); }
                                }
                                capture = Some((cap, reply));
                            }
                        }
                        Some(Cmd::Close) | None => {
                            finish(capture.take(), Err("session closed".into()));
                            let _ = channel.eof().await;
                            let _ = channel.close().await;
                            break;
                        }
                    },
                    // Quiet-based tail end and the overall timeout.
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)), if capture.is_some() => {
                        if let Some(r) = capture.as_mut().and_then(|(c, _)| c.tick()) {
                            finish(capture.take(), r);
                        }
                    }
                    // Flush coalesced output once its window elapses.
                    _ = tokio::time::sleep(flush_due.unwrap_or(std::time::Duration::from_millis(16))), if flush_due.is_some() => {
                        out.flush_timer();
                    }
                }
                // A hidden command just finished: flush any input queued during it.
                if capture.is_none() && !pending_writes.is_empty() {
                    let out = std::mem::take(&mut pending_writes);
                    let _ = channel.data(&out[..]).await;
                }
            }
        });

        let auth_password = match &p.auth {
            Auth::Password(pw) => Some(pw.clone()),
            _ => None,
        };
        Ok(Arc::new(Self { handle: Mutex::new(handle), jumps: Mutex::new(jumps), tx, sftp: Mutex::new(None), auth_password }))
    }
}

/// Collects an exec channel's result. stdout only (stderr is dropped, so probe
/// noise can't masquerade as output), capped; done at Close. ExitStatus usually
/// arrives after Eof, so Eof alone doesn't end it.
#[derive(Default)]
struct ExecOut {
    stdout: Vec<u8>,
    code: Option<u32>,
}

impl ExecOut {
    const CAP: usize = 64 * 1024;

    /// Take one channel message; true when the channel is finished.
    fn feed(&mut self, msg: &ChannelMsg) -> bool {
        match msg {
            ChannelMsg::Data { data } => {
                let room = Self::CAP.saturating_sub(self.stdout.len());
                self.stdout.extend_from_slice(&data[..data.len().min(room)]);
                false
            }
            ChannelMsg::ExitStatus { exit_status } => {
                self.code = Some(*exit_status);
                false
            }
            ChannelMsg::Close => true,
            _ => false,
        }
    }

    /// (stdout, exit code); -1 when the server never sent one.
    fn finish(self) -> (Vec<u8>, i32) {
        (self.stdout, self.code.map(|c| c as i32).unwrap_or(-1))
    }
}

#[cfg(test)]
mod shell_tests {
    use super::*;

    #[test]
    fn unix_shells_start_as_login_shells_others_as_written() {
        assert_eq!(shell_command("zsh"), "zsh -l");
        assert_eq!(shell_command(" /usr/bin/fish "), "/usr/bin/fish -l");
        assert_eq!(shell_command("pwsh"), "pwsh");
        assert_eq!(shell_command("powershell.exe"), "powershell.exe");
        assert_eq!(shell_command("cmd"), "cmd");
        assert_eq!(shell_command("bash --norc"), "bash --norc");
        assert_eq!(shell_command("nu"), "nu");
    }

    #[test]
    fn etc_shells_lists_each_shell_once() {
        let etc = "# /etc/shells: valid login shells\n/bin/sh\n/bin/bash\n/usr/bin/bash\n/bin/rbash\n/usr/bin/zsh\n/usr/sbin/nologin\n\n/usr/bin/tmux\n/usr/bin/fish\n";
        assert_eq!(parse_shells(etc, true), ["/bin/sh", "/bin/bash", "/usr/bin/zsh", "/usr/bin/fish"]);
    }

    #[test]
    fn windows_where_output_gives_shell_names() {
        let out = "C:\\Program Files\\PowerShell\\7\\pwsh.exe\r\nC:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\r\nC:\\Windows\\System32\\cmd.exe\r\n";
        assert_eq!(parse_shells(out, false), ["pwsh", "powershell", "cmd"]);
    }
}

#[cfg(test)]
mod exec_out_tests {
    use super::*;
    use russh::CryptoVec;

    #[test]
    fn collects_stdout_and_exit_status_through_eof_until_close() {
        let mut o = ExecOut::default();
        assert!(!o.feed(&ChannelMsg::Data { data: CryptoVec::from_slice(b"/usr/lib/openssh/") }));
        assert!(!o.feed(&ChannelMsg::ExtendedData { data: CryptoVec::from_slice(b"stderr noise"), ext: 1 }));
        assert!(!o.feed(&ChannelMsg::Data { data: CryptoVec::from_slice(b"sftp-server") }));
        assert!(!o.feed(&ChannelMsg::Eof));
        assert!(!o.feed(&ChannelMsg::ExitStatus { exit_status: 0 }), "exit status after EOF is still read");
        assert!(o.feed(&ChannelMsg::Close));
        assert_eq!(o.finish(), (b"/usr/lib/openssh/sftp-server".to_vec(), 0));
    }

    #[test]
    fn caps_output_and_reports_missing_exit_status() {
        let mut o = ExecOut::default();
        o.feed(&ChannelMsg::Data { data: CryptoVec::from_slice(&vec![b'x'; ExecOut::CAP + 10]) });
        let (out, code) = o.finish();
        assert_eq!(out.len(), ExecOut::CAP);
        assert_eq!(code, -1);
    }
}

#[async_trait]
impl Session for SshSession {
    async fn write(&self, data: &[u8]) -> Result<()> {
        self.tx.send(Cmd::Write(data.to_vec())).await.map_err(|_| anyhow!("channel gone"))
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.tx.send(Cmd::Resize(cols, rows)).await.map_err(|_| anyhow!("channel gone"))
    }

    async fn exec_hidden(&self, cmd: &str) -> Result<(Vec<u8>, i32)> {
        self.exec_hidden_sudo(cmd, false).await
    }

    async fn exec_hidden_sudo(&self, cmd: &str, allow_sudo: bool) -> Result<(Vec<u8>, i32)> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Capture { line: crate::hidden::wrap(cmd).into_bytes(), allow_sudo, reply })
            .await
            .map_err(|_| anyhow!("channel gone"))?;
        rx.await.map_err(|_| anyhow!("session closed"))?
    }

    async fn ssh_password(&self) -> Option<String> {
        self.auth_password.clone()
    }

    async fn exec_channel(&self, cmd: &str) -> Result<(Vec<u8>, i32)> {
        let mut channel = self.handle.lock().await.channel_open_session().await.context("open exec channel")?;
        channel.exec(true, cmd).await.context("exec")?;
        let mut out = ExecOut::default();
        while let Some(msg) = channel.wait().await {
            if out.feed(&msg) {
                break;
            }
        }
        Ok(out.finish())
    }

    async fn sudo_fs(&self, password: &str, server: &str) -> Result<Arc<dyn crate::sftp::Fs>> {
        // New channel; sudo -S reads the password from stdin, -p '' hides the prompt,
        // then execs sftp-server as root on the same channel.
        let channel = self.handle.lock().await.channel_open_session().await.context("open sudo sftp channel")?;
        channel
            .exec(true, format!("sudo -S -p '' {}", crate::hidden::sq(server)))
            .await
            .context("exec sudo sftp-server")?;
        let fs = crate::sftp::sudo_sftp_over(channel.into_stream(), password).await?;
        Ok(Arc::new(fs))
    }

    async fn fs(&self) -> Result<Arc<dyn crate::sftp::Fs>> {
        let mut slot = self.sftp.lock().await;
        if let Some(fs) = slot.as_ref() {
            return Ok(fs.clone());
        }
        // New channel on the existing connection; no re-auth, no second TCP session.
        let channel = self.handle.lock().await.channel_open_session().await.context("open sftp channel")?;
        channel.request_subsystem(true, "sftp").await.context("request sftp subsystem")?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.context("sftp init")?;
        let fs = Arc::new(crate::sftp::SftpFs::new(sftp));
        *slot = Some(fs.clone());
        Ok(fs)
    }

    async fn close(&self) -> Result<()> {
        let _ = self.tx.send(Cmd::Close).await;
        let r = self.handle.lock().await.disconnect(Disconnect::ByApplication, "bye", "en").await;
        // Tear the tunnel down innermost first.
        for h in self.jumps.lock().await.drain(..).rev() {
            let _ = h.disconnect(Disconnect::ByApplication, "bye", "en").await;
        }
        r?;
        Ok(())
    }
}