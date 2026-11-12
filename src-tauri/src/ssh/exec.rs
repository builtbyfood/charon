//! Exec channels on the connection: one-shot commands (probes), root SFTP through
//! `sudo -S sftp-server`.

use anyhow::{Context, Result};
use russh::client::{self, Handle};
use russh::ChannelMsg;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Run `cmd` on a new exec channel (no pty): stdout and the exit code.
pub(super) async fn run<H: client::Handler>(
    handle: &Mutex<Handle<H>>,
    cmd: &str,
) -> Result<(Vec<u8>, i32)> {
    let mut channel = handle
        .lock()
        .await
        .channel_open_session()
        .await
        .context("open exec channel")?;
    channel.exec(true, cmd).await.context("exec")?;
    let mut out = ExecOut::default();
    while let Some(msg) = channel.wait().await {
        if out.feed(&msg) {
            break;
        }
    }
    Ok(out.finish())
}

/// Run `script` as root (MCP `sudo …`), see `Session::exec_root`. A probe first:
/// with NOPASSWD sudo the password is never sent at all. Otherwise it goes to
/// sudo's stdin on this channel, followed by EOF: sudo reads it before running
/// anything (a wrong one fails at once instead of asking again).
pub(super) async fn root<H: client::Handler>(
    handle: &Mutex<Handle<H>>,
    script: &str,
    password: Option<&str>,
) -> Result<(Vec<u8>, i32)> {
    let needs_password = run(handle, crate::hidden::ROOT_PROBE).await?.1 != 0;
    if needs_password && password.is_none() {
        anyhow::bail!("sudo wants a password and this session has none stored (password login, or a vault credential)");
    }
    let mut channel = handle
        .lock()
        .await
        .channel_open_session()
        .await
        .context("open root exec channel")?;
    channel
        .exec(true, crate::hidden::root_exec(script, needs_password))
        .await
        .context("exec sudo")?;
    if let (true, Some(pw)) = (needs_password, password) {
        let mut line = zeroize::Zeroizing::new(pw.as_bytes().to_vec());
        line.push(b'\n');
        channel
            .data(&line[..])
            .await
            .context("send the password to sudo")?;
    }
    channel.eof().await.context("close sudo's stdin")?;
    let mut out = ExecOut {
        stderr_too: true,
        cap: ROOT_CAP,
        ..Default::default()
    };
    while let Some(msg) = channel.wait().await {
        if out.feed(&msg) {
            break;
        }
    }
    Ok(out.finish())
}

/// Output kept from a root command (stdout + stderr).
const ROOT_CAP: usize = 1024 * 1024;

/// Root SFTP: a new channel runs `sudo -S -p '' <server>`; sudo reads the password
/// from stdin (no prompt printed), then execs sftp-server as root on that channel.
pub(super) async fn sudo_sftp<H: client::Handler>(
    handle: &Mutex<Handle<H>>,
    password: &str,
    server: &str,
) -> Result<Arc<dyn crate::sftp::Fs>> {
    let channel = handle
        .lock()
        .await
        .channel_open_session()
        .await
        .context("open sudo sftp channel")?;
    channel
        .exec(true, format!("sudo -S -p '' {}", crate::hidden::sq(server)))
        .await
        .context("exec sudo sftp-server")?;
    let fs = crate::sftp::sudo_sftp_over(channel.into_stream(), password).await?;
    Ok(Arc::new(fs))
}

/// Collects an exec channel's result: stdout (plus stderr when `stderr_too`; a
/// probe drops it so noise can't masquerade as output), capped at `cap` (0 = the
/// 64 KB default); done at Close. ExitStatus usually arrives after Eof, so Eof
/// alone doesn't end it.
#[derive(Default)]
struct ExecOut {
    stdout: Vec<u8>,
    code: Option<u32>,
    stderr_too: bool,
    cap: usize,
}

impl ExecOut {
    const CAP: usize = 64 * 1024;

    fn keep(&mut self, data: &[u8]) {
        let cap = if self.cap == 0 { Self::CAP } else { self.cap };
        let room = cap.saturating_sub(self.stdout.len());
        self.stdout.extend_from_slice(&data[..data.len().min(room)]);
    }

    /// Take one channel message; true when the channel is finished.
    fn feed(&mut self, msg: &ChannelMsg) -> bool {
        match msg {
            ChannelMsg::Data { data } => {
                self.keep(data);
                false
            }
            ChannelMsg::ExtendedData { data, .. } if self.stderr_too => {
                self.keep(data);
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
mod exec_out_tests {
    use super::*;
    fn data(b: &[u8]) -> bytes::Bytes {
        bytes::Bytes::copy_from_slice(b)
    }

    #[test]
    fn collects_stdout_and_exit_status_through_eof_until_close() {
        let mut o = ExecOut::default();
        assert!(!o.feed(&ChannelMsg::Data {
            data: data(b"/usr/lib/openssh/")
        }));
        assert!(!o.feed(&ChannelMsg::ExtendedData {
            data: data(b"stderr noise"),
            ext: 1
        }));
        assert!(!o.feed(&ChannelMsg::Data {
            data: data(b"sftp-server")
        }));
        assert!(!o.feed(&ChannelMsg::Eof));
        assert!(
            !o.feed(&ChannelMsg::ExitStatus { exit_status: 0 }),
            "exit status after EOF is still read"
        );
        assert!(o.feed(&ChannelMsg::Close));
        assert_eq!(o.finish(), (b"/usr/lib/openssh/sftp-server".to_vec(), 0));
    }

    #[test]
    fn caps_output_and_reports_missing_exit_status() {
        let mut o = ExecOut::default();
        o.feed(&ChannelMsg::Data {
            data: data(&vec![b'x'; ExecOut::CAP + 10]),
        });
        let (out, code) = o.finish();
        assert_eq!(out.len(), ExecOut::CAP);
        assert_eq!(code, -1);
    }
}
