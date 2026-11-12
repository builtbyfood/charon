//! End-to-end against a real sshd: connect → run a hidden command → SFTP put/get
//! → root SFTP through `sudo -S sftp-server`. CI's Linux job starts a throwaway
//! sshd in a container (see .github/workflows/build.yml) and sets:
//!
//! - `CHARON_E2E_SSH`  host:port
//! - `CHARON_E2E_USER` / `CHARON_E2E_PASS`  an account that may sudo
//! - `CHARON_E2E_SFTP_SERVER`  sftp-server path (default Debian/Ubuntu's)
//!
//! Without `CHARON_E2E_SSH` the test returns at once (skipped). It drives the same
//! pieces `SshSession` is built from (russh, `authenticate`, `hidden::wrap` +
//! `Capture`, `SftpFs`, `sudo_sftp_over`); the session itself needs a Tauri app.

use super::connect::authenticate;
use super::*;
use crate::sftp::{Fs, SftpFs, Transfer};
use russh::client;
use russh::ChannelMsg;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The container's key is new on every run: accept it (this is a test sshd).
struct AnyKey;

impl client::Handler for AnyKey {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

struct Target {
    host: String,
    port: u16,
    user: String,
    pass: String,
    sftp_server: String,
}

fn target() -> Option<Target> {
    let hp = std::env::var("CHARON_E2E_SSH")
        .ok()
        .filter(|s| !s.is_empty())?;
    let (host, port) = hp.rsplit_once(':')?;
    Some(Target {
        host: host.to_string(),
        port: port.parse().ok()?,
        user: std::env::var("CHARON_E2E_USER").expect("CHARON_E2E_USER"),
        pass: std::env::var("CHARON_E2E_PASS").expect("CHARON_E2E_PASS"),
        sftp_server: std::env::var("CHARON_E2E_SFTP_SERVER")
            .unwrap_or_else(|_| "/usr/lib/openssh/sftp-server".into()),
    })
}

#[tokio::test]
async fn connect_run_sftp_and_sudo_sftp() {
    let Some(t) = target() else {
        eprintln!("CHARON_E2E_SSH not set: SSH end-to-end test skipped");
        return;
    };
    let config = Arc::new(client::Config::default());
    let mut h = client::connect(config, (t.host.as_str(), t.port), AnyKey)
        .await
        .expect("connect");
    authenticate(&mut h, &t.user, &t.host, &Auth::Password(t.pass.clone()))
        .await
        .expect("password auth");

    // Run: a hidden command on an interactive shell. Its output also prints an end
    // sentinel without this command's nonce, which must not end the capture.
    let mut ch = h.channel_open_session().await.expect("session channel");
    ch.request_pty(false, "xterm-256color", 120, 40, 0, 0, &[])
        .await
        .expect("pty");
    ch.request_shell(true).await.expect("shell");
    tokio::time::sleep(Duration::from_millis(800)).await; // let the prompt print
    let nonce = crate::hidden::nonce();
    let cmd = "printf '\\033]7771;e;0\\a'; echo e2e-$((6*7)); id -un; false";
    ch.data(crate::hidden::wrap(cmd, &nonce).as_bytes())
        .await
        .expect("type the command");
    let mut cap = crate::hidden::Capture::new(&nonce);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            m = ch.wait() => match m {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => { cap.feed(&data); }
                None => panic!("shell channel closed"),
                _ => {}
            },
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        if let Some(r) = cap.tick() {
            r.expect("capture");
            break;
        }
        assert!(Instant::now() < deadline, "hidden command did not finish");
    }
    let (out, code) = cap.into_result();
    let text = crate::hidden::strip_escapes(&String::from_utf8_lossy(&out));
    assert!(text.contains("e2e-42"), "{text:?}");
    assert!(text.contains(&t.user), "{text:?}");
    assert_eq!(code, 1, "exit code of the last command (false)");

    // SFTP: put a file and get it back.
    let ch = h.channel_open_session().await.expect("sftp channel");
    ch.request_subsystem(true, "sftp")
        .await
        .expect("sftp subsystem");
    let fs = SftpFs::new(
        russh_sftp::client::SftpSession::new(ch.into_stream())
            .await
            .expect("sftp session"),
    );
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(7) >> 3) as u8)
        .collect();
    let up = dir.path().join("up.bin");
    std::fs::write(&up, &data).unwrap();
    let remote = fs.join(&fs.home().await.expect("home"), "charon-e2e.bin");
    let tr = Transfer::new("e2e".into(), None, Arc::new(AtomicBool::new(false)));
    fs.upload(&up, &remote, &tr).await.expect("upload");
    let down = dir.path().join("down.bin");
    fs.download(&remote, &down, &tr).await.expect("download");
    assert_eq!(std::fs::read(&down).unwrap(), data, "round trip");
    fs.delete(&remote, false).await.expect("delete");

    // Root SFTP over `sudo -S sftp-server` on its own channel (as `SshSession::sudo_fs`).
    let ch = h.channel_open_session().await.expect("sudo channel");
    ch.exec(
        true,
        format!("sudo -S -p '' {}", crate::hidden::sq(&t.sftp_server)),
    )
    .await
    .expect("exec sudo");
    let root = crate::sftp::sudo_sftp_over(ch.into_stream(), &t.pass)
        .await
        .expect("sudo sftp");
    root.list("/root").await.expect("list /root as root");
    root.write("/root/charon-e2e.txt", b"as root")
        .await
        .expect("write as root");
    assert_eq!(
        root.read("/root/charon-e2e.txt", 1024)
            .await
            .expect("read as root"),
        b"as root"
    );
    root.delete("/root/charon-e2e.txt", false)
        .await
        .expect("delete as root");

    // MCP root path: `sudo …` on its own channel, password to sudo's stdin only.
    let h = tokio::sync::Mutex::new(h);
    let (out, code) = super::exec::root(&h, "id -un", Some(&t.pass))
        .await
        .expect("root exec");
    assert_eq!(String::from_utf8_lossy(&out).trim(), "root");
    assert_eq!(code, 0);
    // A command that prints a fake sudo prompt and reads an answer gets nothing:
    // Charon doesn't watch the output, and the script's stdin is /dev/null.
    let fake =
        "printf '[sudo] password for %s: ' \"$SUDO_USER\"; IFS= read -r x; echo \"got=[$x]\"";
    let (out, _) = super::exec::root(&h, fake, Some(&t.pass))
        .await
        .expect("root exec");
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("got=[]"), "{text:?}");
    assert!(
        !text.contains(&t.pass),
        "the password never reaches the command"
    );
    // A wrong password fails at once instead of waiting for another try.
    let started = Instant::now();
    let (_, code) = super::exec::root(&h, "true", Some("not-the-password"))
        .await
        .expect("root exec");
    assert_ne!(code, 0);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "no second password prompt"
    );
}
