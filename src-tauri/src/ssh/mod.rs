//! SSH shell session on top of russh. Password and key auth; host keys are
//! checked against known_hosts (see `hostkeys.rs`) with a trust-on-first-use
//! prompt routed through the UI. Jump hosts: each hop is a full SSH session
//! and the next hop rides a direct-tcpip channel through it (like ProxyJump).
//!
//! - `connect`: config, jump hops, the host-key `Handler`, authentication
//! - `channel`: the task that owns the interactive shell channel (output pump,
//!   input queue, hidden-command capture)
//! - `exec`: exec channels (probes, sudo-sftp, MCP commands run as root)

mod channel;
mod connect;
mod exec;

use crate::session::Session;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use russh::client::Handle;
use russh::Disconnect;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};

use channel::Cmd;
use connect::Handler;

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
const LOGIN_SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "ksh", "mksh", "dash", "ash", "tcsh", "csh", "yash", "oksh",
];

fn shell_base(s: &str) -> String {
    let b = s
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(s)
        .to_ascii_lowercase();
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
    const SKIP: &[&str] = &[
        "nologin",
        "false",
        "git-shell",
        "rbash",
        "screen",
        "tmux",
        "sync",
        "true",
    ];
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
    Key {
        key: String,
        passphrase: Option<String>,
    },
}

impl Auth {
    fn load_key(key: &str, passphrase: Option<&str>) -> Result<russh::keys::PrivateKey> {
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
            russh::keys::decode_secret_key(&text, passphrase)
                .with_context(|| format!("decode private key {key}"))
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
            Self::HostKeyTimeout => write!(
                f,
                "no answer to the host key prompt within {:?}; connection dropped",
                connect::HOSTKEY_PROMPT_TIMEOUT
            ),
        }
    }
}

impl std::error::Error for SshError {}

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

#[async_trait]
impl Session for SshSession {
    async fn write(&self, data: &[u8]) -> Result<()> {
        self.tx
            .send(Cmd::Write(data.to_vec()))
            .await
            .map_err(|_| anyhow!("channel gone"))
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.tx
            .send(Cmd::Resize(cols, rows))
            .await
            .map_err(|_| anyhow!("channel gone"))
    }

    async fn exec_hidden(&self, cmd: &str) -> Result<(Vec<u8>, i32)> {
        let (reply, rx) = oneshot::channel();
        let nonce = crate::hidden::nonce();
        self.tx
            .send(Cmd::Capture {
                line: crate::hidden::wrap(cmd, &nonce).into_bytes(),
                nonce,
                reply,
            })
            .await
            .map_err(|_| anyhow!("channel gone"))?;
        rx.await.map_err(|_| anyhow!("session closed"))?
    }

    async fn ssh_password(&self) -> Option<String> {
        self.auth_password.clone()
    }

    async fn exec_channel(&self, cmd: &str) -> Result<(Vec<u8>, i32)> {
        exec::run(&self.handle, cmd).await
    }

    async fn exec_root(&self, script: &str, password: Option<&str>) -> Result<(Vec<u8>, i32)> {
        exec::root(&self.handle, script, password).await
    }

    async fn sudo_fs(&self, password: &str, server: &str) -> Result<Arc<dyn crate::sftp::Fs>> {
        exec::sudo_sftp(&self.handle, password, server).await
    }

    async fn fs(&self) -> Result<Arc<dyn crate::sftp::Fs>> {
        let mut slot = self.sftp.lock().await;
        if let Some(fs) = slot.as_ref() {
            return Ok(fs.clone());
        }
        // New channel on the existing connection; no re-auth, no second TCP session.
        let channel = self
            .handle
            .lock()
            .await
            .channel_open_session()
            .await
            .context("open sftp channel")?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .context("request sftp subsystem")?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .context("sftp init")?;
        let fs = Arc::new(crate::sftp::SftpFs::new(sftp));
        *slot = Some(fs.clone());
        Ok(fs)
    }

    async fn close(&self) -> Result<()> {
        let _ = self.tx.send(Cmd::Close).await;
        let r = self
            .handle
            .lock()
            .await
            .disconnect(Disconnect::ByApplication, "bye", "en")
            .await;
        // Tear the tunnel down innermost first.
        for h in self.jumps.lock().await.drain(..).rev() {
            let _ = h.disconnect(Disconnect::ByApplication, "bye", "en").await;
        }
        r?;
        Ok(())
    }
}

#[cfg(test)]
mod e2e;

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
        assert_eq!(
            parse_shells(etc, true),
            ["/bin/sh", "/bin/bash", "/usr/bin/zsh", "/usr/bin/fish"]
        );
    }

    #[test]
    fn windows_where_output_gives_shell_names() {
        let out = "C:\\Program Files\\PowerShell\\7\\pwsh.exe\r\nC:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\r\nC:\\Windows\\System32\\cmd.exe\r\n";
        assert_eq!(parse_shells(out, false), ["pwsh", "powershell", "cmd"]);
    }
}
