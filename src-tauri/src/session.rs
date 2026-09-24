//! Session registry. A session is anything that produces bytes and accepts bytes:
//! SSH shell, local PTY, serial port. The UI only ever sees the `id`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex};

#[async_trait]
pub trait Session: Send + Sync {
    async fn write(&self, data: &[u8]) -> Result<()>;
    async fn resize(&self, cols: u16, rows: u16) -> Result<()>;
    async fn close(&self) -> Result<()>;
    /// File access for the SFTP pane / editor, if this kind of session has any
    /// (SFTP subsystem for SSH, the local disk for a local PTY).
    async fn fs(&self) -> Result<Arc<dyn crate::sftp::Fs>> {
        Err(anyhow!("this session has no file access"))
    }
    /// Run a command on the interactive shell without the user seeing it (see
    /// `hidden.rs`): returns its output and exit code. Only SSH shells do this.
    async fn exec_hidden(&self, _cmd: &str) -> Result<(Vec<u8>, i32)> {
        Err(anyhow!("this session cannot run hidden commands"))
    }
    /// Like `exec_hidden`, but `allow_sudo` permits auto-answering a
    /// `[sudo] password for <login-user>:` prompt with the session's password
    /// (MCP run_command, tier-gated). Default ignores the flag.
    async fn exec_hidden_sudo(&self, cmd: &str, _allow_sudo: bool) -> Result<(Vec<u8>, i32)> {
        self.exec_hidden(cmd).await
    }
    /// Open a root SFTP channel via `sudo -S <server>` (P6). SSH only.
    async fn sudo_fs(&self, _password: &str, _server: &str) -> Result<Arc<dyn crate::sftp::Fs>> {
        Err(anyhow!("this session has no sudo file access"))
    }
    /// The password this session authenticated with, if password auth — offered
    /// (never shown) to sudo so the user need not retype it.
    async fn ssh_password(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub kind: String,
    /// Inject the OSC 133 hook once the first prompt is seen.
    #[serde(default)]
    pub shell_integration: bool,
    /// Shell detected by the UI ("bash", "zsh", "fish", "pwsh"), for the docs feature.
    #[serde(default)]
    pub shell: Option<String>,
    /// Working directory reported by the shell over OSC 7 (SFTP "follow shell").
    #[serde(default)]
    pub cwd: Option<String>,
    /// Saved node this session came from (for MCP tier resolution); None for ad-hoc.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Remote host, for MCP `list_sessions`.
    #[serde(default)]
    pub host: Option<String>,
}

impl SessionInfo {
    pub fn new(id: String, title: String, kind: &str) -> Self {
        Self { id, title, kind: kind.into(), shell_integration: false, shell: None, cwd: None, node_id: None, host: None }
    }
}

#[derive(Clone)]
pub struct SessionManager {
    app: AppHandle,
    inner: Arc<Mutex<HashMap<String, (SessionInfo, Arc<dyn Session>)>>>,
    /// Last time the user typed into a session (for the MCP run_command guard).
    last_write: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// Sessions mid-connect waiting for the user to accept/reject a host key.
    hostkey_prompts: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl SessionManager {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            inner: Arc::new(Mutex::new(HashMap::new())),
            last_write: Arc::new(Mutex::new(HashMap::new())),
            hostkey_prompts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register that session `id` is about to ask about a host key; the receiver
    /// resolves when `answer_hostkey` is called (or is dropped if it never is).
    pub async fn expect_hostkey_answer(&self, id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.hostkey_prompts.lock().await.insert(id.to_string(), tx);
        rx
    }

    pub async fn answer_hostkey(&self, id: &str, accept: bool) -> Result<()> {
        let mut pending = self.hostkey_prompts.lock().await;
        let Some(tx) = pending.remove(id) else {
            let waiting: Vec<&String> = pending.keys().collect();
            tracing::warn!(session = %id, accept, ?waiting, "host key answer for a session that is not waiting");
            return Err(anyhow!("no host key prompt is waiting for session {id}"));
        };
        drop(pending);
        tracing::info!(session = %id, accept, "host key answer received");
        tx.send(accept).map_err(|_| anyhow!("session {id} gave up waiting"))
    }

    pub async fn cancel_hostkey_prompt(&self, id: &str) {
        self.hostkey_prompts.lock().await.remove(id);
    }

    pub fn app(&self) -> &AppHandle {
        &self.app
    }

    pub async fn register(&self, info: SessionInfo, s: Arc<dyn Session>) {
        self.inner.lock().await.insert(info.id.clone(), (info, s));
    }

    pub async fn get(&self, id: &str) -> Result<Arc<dyn Session>> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|(_, s)| s.clone())
            .ok_or_else(|| anyhow!("no session {id}"))
    }

    pub async fn remove(&self, id: &str) {
        self.inner.lock().await.remove(id);
    }

    pub async fn set_shell(&self, id: &str, shell: Option<String>) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let (info, _) = inner.get_mut(id).ok_or_else(|| anyhow!("no session {id}"))?;
        info.shell = shell;
        Ok(())
    }

    pub async fn set_cwd(&self, id: &str, cwd: Option<String>) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let (info, _) = inner.get_mut(id).ok_or_else(|| anyhow!("no session {id}"))?;
        info.cwd = cwd;
        Ok(())
    }

    pub async fn info(&self, id: &str) -> Option<SessionInfo> {
        self.inner.lock().await.get(id).map(|(i, _)| i.clone())
    }

    /// Record that the user just typed into `id` (called from `session_write`).
    pub async fn mark_user_write(&self, id: &str) {
        self.last_write.lock().await.insert(id.to_string(), std::time::Instant::now());
    }

    /// True if the user typed into `id` within `within` (so an injected hidden
    /// command would clobber what they're typing).
    pub async fn typed_recently(&self, id: &str, within: std::time::Duration) -> bool {
        self.last_write.lock().await.get(id).map(|t| t.elapsed() < within).unwrap_or(false)
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        self.inner.lock().await.values().map(|(i, _)| i.clone()).collect()
    }

    /// Push output bytes to the UI. Base64 so binary-safe through JSON.
    pub fn emit_data(app: &AppHandle, id: &str, data: &[u8]) {
        let _ = app.emit(
            &format!("term:data:{id}"),
            base64::engine::general_purpose::STANDARD.encode(data),
        );
    }

    pub fn emit_closed(app: &AppHandle, id: &str, reason: &str) {
        let _ = app.emit(&format!("term:closed:{id}"), reason.to_string());
    }
}

/// Batches a session's output so the UI gets at most one `term:data` event per
/// window (16 ms), instead of one per SSH packet / PTY read. The first byte of a
/// burst is emitted immediately (low latency); subsequent bytes within the window
/// accumulate and flush together (fewer IPC round-trips + JSON/base64 encodes).
pub struct Coalescer {
    app: AppHandle,
    id: String,
    buf: Vec<u8>,
    last_flush: std::time::Instant,
    window: std::time::Duration,
}

impl Coalescer {
    pub fn new(app: AppHandle, id: String) -> Self {
        // Start "due" so the first push flushes at once.
        let window = std::time::Duration::from_millis(16);
        Self { app, id, buf: Vec::new(), last_flush: std::time::Instant::now() - window, window }
    }

    /// Append output; flush now if the window has elapsed since the last flush.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.last_flush.elapsed() >= self.window {
            self.flush();
        }
    }

    /// Time until a buffered flush is due, or `None` when nothing is pending.
    pub fn due_in(&self) -> Option<std::time::Duration> {
        if self.buf.is_empty() {
            None
        } else {
            Some(self.window.saturating_sub(self.last_flush.elapsed()))
        }
    }

    pub fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        SessionManager::emit_data(&self.app, &self.id, &self.buf);
        self.buf.clear();
        self.last_flush = std::time::Instant::now();
    }
}
