//! Opening the connection: jump hops, the host-key check, authentication.

use super::{channel, Auth, HostKeyChanged, HostKeyPrompt, SshError, SshParams, SshSession};
use crate::hostkeys::{fingerprint, HostKeys, Verdict};
use crate::session::SessionManager;
use anyhow::{anyhow, Context, Result};
use russh::client::{self, Handle};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{mpsc, Mutex};

/// How long one authentication step may take (a wrong password costs the server's
/// failure delay, a few seconds) before we stop waiting for a server that went quiet.
const AUTH_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the user gets to answer the host key prompt before we give up.
pub(super) const HOSTKEY_PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub(super) struct Handler {
    mgr: SessionManager,
    id: String,
    host: String,
    port: u16,
    keys: Arc<HostKeys>,
}

impl client::Handler for Handler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // A host certificate is checked like the key it certifies (no CA trust).
        let cert_key;
        let key: &russh::keys::PublicKey = match server_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key,
            russh::keys::PublicKeyOrCertificate::Certificate(c) => {
                cert_key = russh::keys::PublicKey::new(c.public_key().clone(), "");
                &cert_key
            }
        };
        let verdict = self.keys.check(&self.host, self.port, key);
        tracing::info!(session = %self.id, host = %self.host, port = self.port, key_type = key.algorithm().as_str(),
            fingerprint = %fingerprint(key), ?verdict, "host key check");
        match verdict {
            Verdict::Known => Ok(true),
            Verdict::Changed { file, line } => Err(SshError::HostKeyChanged(HostKeyChanged {
                host: self.host.clone(),
                port: self.port,
                key_type: key.algorithm().as_str().to_string(),
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
                    key_type: key.algorithm().as_str().to_string(),
                    fingerprint: fingerprint(key),
                    known_hosts: self.keys.own_path().display().to_string(),
                };
                let event = format!("ssh:hostkey:{}", self.id);
                let sent = self.mgr.app().emit(&event, prompt);
                crate::session::wake_ui();
                match sent {
                    Ok(()) => {
                        tracing::info!(session = %self.id, %event, "host key prompt emitted; waiting for answer")
                    }
                    Err(e) => {
                        tracing::error!(session = %self.id, %event, "could not emit host key prompt: {e}")
                    }
                }
                let answer = tokio::time::timeout(HOSTKEY_PROMPT_TIMEOUT, rx).await;
                self.mgr.cancel_hostkey_prompt(&self.id).await;
                match answer {
                    Ok(Ok(true)) => {
                        match self.keys.learn(&self.host, self.port, key) {
                            Ok(()) => {
                                tracing::info!(session = %self.id, file = %self.keys.own_path().display(), "host key accepted and recorded")
                            }
                            Err(e) => {
                                tracing::warn!(session = %self.id, "host key accepted but not recorded: {e:#}")
                            }
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

/// Authenticate an open handle; the error names the host so a failed hop is obvious.
pub(super) async fn authenticate<H: client::Handler>(
    handle: &mut Handle<H>,
    user: &str,
    host: &str,
    auth: &Auth,
) -> Result<()> {
    let authed = match auth {
        Auth::Key { key, passphrase } => {
            let key = Auth::load_key(key, passphrase.as_deref())?;
            step(
                async {
                    // RSA: the hash the server lists (server-sig-algs). No list at all:
                    // rsa-sha2-512, as before; SHA-1 only for a server that lists none.
                    let hash = match handle.best_supported_rsa_hash().await? {
                        Some(h) => h,
                        None => Some(russh::keys::HashAlg::Sha512),
                    };
                    let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash);
                    Ok(handle.authenticate_publickey(user, key).await?.success())
                },
                "public key",
            )
            .await?
        }
        Auth::Password(pw) => {
            // Keyboard-interactive first, then the plain "password" method. Many
            // servers (HPE iLO, UniFi OS, any OpenSSH with `PasswordAuthentication no`
            // + `KbdInteractiveAuthentication yes`) take a password *only* through
            // keyboard-interactive. The other order hangs with russh 0.45 on those:
            // after the server refuses "password", the keyboard-interactive request
            // never gets an answer (seen on a UDM and a NAS, both OpenSSH 8.4+).
            // A server without keyboard-interactive answers Failure at once.
            if step(
                keyboard_interactive(handle, user, pw),
                "keyboard-interactive",
            )
            .await?
            {
                true
            } else {
                step(
                    async { Ok(handle.authenticate_password(user, pw).await?.success()) },
                    "password",
                )
                .await?
            }
        }
    };
    if !authed {
        return Err(anyhow!("authentication rejected for {user}@{host}"));
    }
    Ok(())
}

/// One auth exchange, bounded by `AUTH_STEP_TIMEOUT`.
async fn step<F: std::future::Future<Output = Result<bool>>>(f: F, what: &str) -> Result<bool> {
    tokio::time::timeout(AUTH_STEP_TIMEOUT, f)
        .await
        .map_err(|_| {
            anyhow!(
                "no answer from the server during {what} authentication ({}s)",
                AUTH_STEP_TIMEOUT.as_secs()
            )
        })?
}

/// Answer keyboard-interactive prompts with the password. This covers the common
/// single-factor case (one non-echo "Password:" prompt); a genuine second factor
/// (an OTP prompt we can't fill from a stored password) is left to fail cleanly
/// rather than loop. Echoed prompts get an empty answer so the password is never
/// sent to a visible field.
async fn keyboard_interactive<H: client::Handler>(
    handle: &mut Handle<H>,
    user: &str,
    pw: &str,
) -> Result<bool> {
    use russh::client::KeyboardInteractiveAuthResponse as Kir;
    let mut resp = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await?;
    for _ in 0..10 {
        match resp {
            Kir::Success => return Ok(true),
            Kir::Failure { .. } => return Ok(false),
            Kir::InfoRequest { prompts, .. } => {
                let answers = prompts
                    .iter()
                    .map(|p| {
                        if p.echo {
                            String::new()
                        } else {
                            pw.to_string()
                        }
                    })
                    .collect();
                resp = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(false)
}

impl SshSession {
    pub async fn connect(
        mgr: SessionManager,
        keys: Arc<HostKeys>,
        id: String,
        p: SshParams,
    ) -> Result<Arc<Self>> {
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
            let next = p
                .jumps
                .get(i + 1)
                .map(|h| (h.host.as_str(), h.port))
                .unwrap_or((p.host.as_str(), p.port));
            tracing::info!(session = %id, hop = %hop.host, port = hop.port, next = %next.0, "connecting via jump host");
            let mut h = match tunnel.take() {
                None => {
                    client::connect(
                        config.clone(),
                        (hop.host.as_str(), hop.port),
                        handler(&hop.host, hop.port),
                    )
                    .await
                }
                Some(stream) => {
                    client::connect_stream(config.clone(), stream, handler(&hop.host, hop.port))
                        .await
                }
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
            None => {
                client::connect(config, (p.host.as_str(), p.port), handler(&p.host, p.port)).await
            }
            Some(stream) => client::connect_stream(config, stream, handler(&p.host, p.port)).await,
        }
        .with_context(|| format!("connect {}:{}", p.host, p.port))?;
        authenticate(&mut handle, &p.user, &p.host, &p.auth).await?;

        let channel = handle.channel_open_session().await?;
        channel
            .request_pty(
                false,
                "xterm-256color",
                p.cols as u32,
                p.rows as u32,
                0,
                0,
                &[],
            )
            .await?;
        match p.shell.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(shell) => {
                let cmd = super::shell_command(shell);
                tracing::info!(session = %id, shell = %cmd, "starting chosen shell");
                channel.exec(true, cmd).await?;
            }
            None => channel.request_shell(true).await?,
        }

        let (tx, rx) = mpsc::channel::<channel::Cmd>(256);
        channel::spawn(app, id, channel, rx);

        let auth_password = match &p.auth {
            Auth::Password(pw) => Some(pw.clone()),
            _ => None,
        };
        Ok(Arc::new(Self {
            handle: Mutex::new(handle),
            jumps: Mutex::new(jumps),
            tx,
            sftp: Mutex::new(None),
            auth_password,
        }))
    }
}
