//! MCP server: lets an agent (Claude Code, Cursor, …) drive the running app's
//! sessions. The wire protocol is MCP's JSON-RPC 2.0 (see `rpc`), implemented
//! directly rather than via a fast-moving SDK so it stays testable here. Two
//! transports (`http`, `stdio`) both call `dispatch`; the app is the single
//! owner of sessions.
//!
//! Permission tiers come from each session's node (`Settings.mcp_tier`,
//! inherited); without one, `mcp.default_tier`, or for local shells
//! `local.mcp_tier` (Local (admin): read). `read` = list/read/get_connection only; `exec-approve` =
//! run_command / sftp_put pop an in-app approval; `trusted` = no prompt. Every
//! call is appended to `data/mcp.log`.

pub mod http;
pub mod rpc;
pub mod stdio;
pub mod tools;

use crate::AppState;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};

/// Approval waits this long before defaulting to deny.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// Frontend round-trips (read_scrollback, open_in_editor) time out here.
pub const BRIDGE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Read,
    ExecApprove,
    Trusted,
}

impl Tier {
    pub fn parse(s: &str) -> Tier {
        match s {
            "trusted" => Tier::Trusted,
            "exec-approve" => Tier::ExecApprove,
            _ => Tier::Read,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::ExecApprove => "exec-approve",
            Tier::Trusted => "trusted",
        }
    }
}

/// What a tool does, for the tier gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// list/get/read — always allowed.
    Read,
    /// touches files (sftp_get, open_in_editor) — needs ≥ exec-approve, no prompt.
    File,
    /// run_command / sftp_put — prompts at exec-approve, allowed at trusted.
    Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    NeedApproval,
}

/// The tier gate, pure and tested.
pub fn gate(kind: ToolKind, tier: Tier) -> Decision {
    match (kind, tier) {
        (ToolKind::Read, _) => Decision::Allow,
        (_, Tier::Trusted) => Decision::Allow,
        (ToolKind::File, Tier::ExecApprove) => Decision::Allow,
        (ToolKind::Exec, Tier::ExecApprove) => Decision::NeedApproval,
        (_, Tier::Read) => Decision::Deny,
    }
}

/// Runtime state of the MCP server, held in `AppState`.
pub struct McpState {
    /// Bearer token for the HTTP transport (regenerated on demand / at start).
    pub token: Mutex<String>,
    /// Bound port once the server is listening.
    pub port: Mutex<Option<u16>>,
    running: AtomicBool,
    stop_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Pending approvals / frontend round-trips, answered by commands.
    pending: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
    /// Sessions the user chose "always allow for this session" on.
    always: Mutex<HashSet<String>>,
    /// Sessions with an MCP command in flight (per-session run_command lock).
    busy: std::sync::Mutex<HashSet<String>>,
}

impl Default for McpState {
    fn default() -> Self {
        Self {
            token: Mutex::new(gen_token()),
            port: Mutex::new(None),
            running: AtomicBool::new(false),
            stop_tx: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            always: Mutex::new(HashSet::new()),
            busy: std::sync::Mutex::new(HashSet::new()),
        }
    }
}

impl McpState {
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Register a pending frontend round-trip; the returned receiver resolves when
    /// `answer` is called with the same id (or is dropped on timeout).
    pub async fn expect(&self, id: &str) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.to_string(), tx);
        rx
    }

    pub async fn answer(&self, id: &str, value: serde_json::Value) {
        if let Some(tx) = self.pending.lock().await.remove(id) {
            let _ = tx.send(value);
        }
    }

    pub async fn cancel(&self, id: &str) {
        self.pending.lock().await.remove(id);
    }

    pub async fn always_allows(&self, session: &str) -> bool {
        self.always.lock().await.contains(session)
    }

    pub async fn remember_always(&self, session: &str) {
        self.always.lock().await.insert(session.to_string());
    }

    /// Serialise MCP commands per session: `None` = one is already running.
    pub fn try_busy(self: &Arc<Self>, session: &str) -> Option<BusyGuard> {
        let mut b = self.busy.lock().unwrap();
        if b.contains(session) {
            return None;
        }
        b.insert(session.to_string());
        Some(BusyGuard {
            state: self.clone(),
            session: session.to_string(),
        })
    }
}

/// Releases the per-session run lock on drop.
pub struct BusyGuard {
    state: Arc<McpState>,
    session: String,
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.state.busy.lock().unwrap().remove(&self.session);
    }
}

/// 32 hex chars from the OS RNG (via uuid, which we already depend on).
pub fn gen_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )[..48]
        .to_string()
}

/// A line for `data/mcp.log`. Deliberately carries NO command text, arguments,
/// output, or credential material — only who did what and the tier decision.
#[derive(Serialize)]
struct LogLine<'a> {
    ts: String,
    tool: &'a str,
    session: Option<&'a str>,
    tier: &'a str,
    /// "allow" | "deny" | "approved" | "denied"
    decision: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    duration_ms: u128,
    /// What the agent asked for: the command line, or the path(s) touched.
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<&'a str>,
    /// Why the call failed, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

/// Longest `subject` / `error` kept per log line.
const LOG_FIELD_MAX: usize = 2000;

fn clip(s: &str) -> &str {
    match s.char_indices().nth(LOG_FIELD_MAX) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

/// One line in data/mcp.log per tool call, including denied ones. `subject` is
/// what was asked for (see `tools::subject`), so an agent's command can be traced
/// back to the session it ran in.
#[allow(clippy::too_many_arguments)]
pub async fn log_call(
    state: &AppState,
    tool: &str,
    session: Option<&str>,
    tier: Tier,
    decision: &str,
    ok: bool,
    exit_code: Option<i32>,
    duration_ms: u128,
    subject: Option<&str>,
    error: Option<&str>,
) {
    let line = LogLine {
        ts: now_iso(),
        tool,
        session,
        tier: tier.as_str(),
        decision,
        ok,
        exit_code,
        duration_ms,
        subject: subject.map(clip),
        error: error.map(clip),
    };
    if let Ok(mut s) = serde_json::to_string(&line) {
        s.push('\n');
        let path = state.paths.data_dir.join("mcp.log");
        use tokio::io::AsyncWriteExt;
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            let _ = f.write_all(s.as_bytes()).await;
        }
    }
}

fn now_iso() -> String {
    // Cheap UTC timestamp without pulling chrono: seconds since epoch is enough for a log.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

pub type McpShared = Arc<McpState>;

// ---------------------------------------------------------------- control

/// {port, token} written beside the data so the stdio shim can reach the server.
fn endpoint_path(state: &AppState) -> std::path::PathBuf {
    state.paths.data_dir.join("mcp-endpoint.json")
}

/// Start the HTTP server (idempotent). Returns the bound port.
pub async fn start(app: &tauri::AppHandle, state: &AppState, port: u16) -> anyhow::Result<u16> {
    if state.mcp.is_running() {
        return Ok(state.mcp.port.lock().await.unwrap_or(0));
    }
    let (tx, rx) = oneshot::channel();
    let bound = http::serve(app.clone(), state.mcp.clone(), port, rx).await?;
    *state.mcp.stop_tx.lock().await = Some(tx);
    state.mcp.running.store(true, Ordering::SeqCst);
    let token = state.mcp.token.lock().await.clone();
    let ep = serde_json::json!({ "port": bound, "token": token });
    let ep_path = endpoint_path(state);
    // The token is a secret: the file is the user's alone before the token goes in.
    if let Err(e) = write_private(&ep_path, &serde_json::to_vec(&ep).unwrap_or_default()) {
        tracing::warn!("mcp endpoint file: {e:#}");
    }
    tracing::info!(port = bound, "mcp server started");
    Ok(bound)
}

/// Stop the server and remove the endpoint file.
pub async fn stop(state: &AppState) {
    if let Some(tx) = state.mcp.stop_tx.lock().await.take() {
        let _ = tx.send(());
    }
    state.mcp.running.store(false, Ordering::SeqCst);
    *state.mcp.port.lock().await = None;
    let _ = tokio::fs::remove_file(endpoint_path(state)).await;
    tracing::info!("mcp server stopped");
}

/// Write `bytes` to a file only the current user can read, restricting it before
/// anything is written: mode 0600 on Unix (also on a file left from an earlier
/// run); on Windows the inherited ACL is replaced by one granting the user alone
/// (a portable data dir beside the exe may be readable by others).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(bytes)?;
    }
    #[cfg(windows)]
    {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        restrict_acl(path)?;
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)?
            .write_all(bytes)?;
    }
    Ok(())
}

/// Windows: drop inherited ACEs and grant only the current user full control.
#[cfg(windows)]
fn restrict_acl(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) if !d.is_empty() => format!(r"{d}\{u}"),
        (_, Ok(u)) => u,
        _ => anyhow::bail!("no USERNAME to grant"),
    };
    let out = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Compare a presented token with the real one in time independent of where they differ.
pub fn token_matches(presented: &str, token: &str) -> bool {
    let (a, b) = (presented.as_bytes(), token.as_bytes());
    if a.len() != b.len() || b.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The endpoint the stdio shim connects to (written by `start`).
pub fn read_endpoint(data_dir: &std::path::Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(data_dir.join("mcp-endpoint.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((
        v.get("port")?.as_u64()? as u16,
        v.get("token")?.as_str()?.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_gate() {
        // read tier: only Read tools.
        assert_eq!(gate(ToolKind::Read, Tier::Read), Decision::Allow);
        assert_eq!(gate(ToolKind::File, Tier::Read), Decision::Deny);
        assert_eq!(gate(ToolKind::Exec, Tier::Read), Decision::Deny);
        // exec-approve: reads + files free, exec prompts.
        assert_eq!(gate(ToolKind::Read, Tier::ExecApprove), Decision::Allow);
        assert_eq!(gate(ToolKind::File, Tier::ExecApprove), Decision::Allow);
        assert_eq!(
            gate(ToolKind::Exec, Tier::ExecApprove),
            Decision::NeedApproval
        );
        // trusted: everything, no prompt.
        assert_eq!(gate(ToolKind::Exec, Tier::Trusted), Decision::Allow);
    }

    #[test]
    fn tier_parse_defaults_to_read() {
        assert_eq!(Tier::parse("trusted"), Tier::Trusted);
        assert_eq!(Tier::parse("exec-approve"), Tier::ExecApprove);
        assert_eq!(Tier::parse("read"), Tier::Read);
        assert_eq!(Tier::parse("nonsense"), Tier::Read);
    }

    #[test]
    fn run_lock_is_per_session_and_released_on_drop() {
        let mcp = std::sync::Arc::new(McpState::default());
        let g1 = mcp.try_busy("s1");
        assert!(g1.is_some());
        assert!(mcp.try_busy("s1").is_none(), "second lock on s1 is refused");
        assert!(
            mcp.try_busy("s2").is_some(),
            "a different session is independent"
        );
        drop(g1);
        assert!(mcp.try_busy("s1").is_some(), "lock frees on drop");
    }

    #[test]
    fn token_compare() {
        let t = gen_token();
        assert!(token_matches(&t, &t));
        assert!(!token_matches(&t[..47], &t));
        assert!(!token_matches(&format!("{}x", &t[..47]), &t));
        assert!(!token_matches("", ""), "an empty token never matches");
    }

    #[test]
    fn endpoint_file_is_private_and_holds_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp-endpoint.json");
        std::fs::write(&p, b"old, longer contents than the new ones").unwrap();
        write_private(&p, b"{\"port\":1}").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"port\":1}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        {
            let out = std::process::Command::new("icacls")
                .arg(&p)
                .output()
                .unwrap();
            let acl = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(!acl.contains("(I)"), "no inherited entries: {acl}");
            assert!(
                !acl.contains("Everyone") && !acl.contains(r"BUILTIN\Users"),
                "{acl}"
            );
        }
    }

    #[test]
    fn token_is_hex_and_long() {
        let t = gen_token();
        assert_eq!(t.len(), 48);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
