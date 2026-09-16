//! MCP server: lets an agent (Claude Code, Cursor, …) drive the running app's
//! sessions. The wire protocol is MCP's JSON-RPC 2.0 (see `rpc`), implemented
//! directly rather than via a fast-moving SDK so it stays testable here. Two
//! transports (`http`, `stdio`) both call `dispatch`; the app is the single
//! owner of sessions.
//!
//! Permission tiers come from each session's node (`Settings.mcp_tier`,
//! inherited): `read` = list/read/get_connection only; `exec-approve` =
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
        Some(BusyGuard { state: self.clone(), session: session.to_string() })
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
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())[..48].to_string()
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
}

pub async fn log_call(state: &AppState, tool: &str, session: Option<&str>, tier: Tier, decision: &str, ok: bool, exit_code: Option<i32>, duration_ms: u128) {
    let line = LogLine { ts: now_iso(), tool, session, tier: tier.as_str(), decision, ok, exit_code, duration_ms };
    if let Ok(mut s) = serde_json::to_string(&line) {
        s.push('\n');
        let path = state.paths.data_dir.join("mcp.log");
        use tokio::io::AsyncWriteExt;
        if let Ok(mut f) = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await {
            let _ = f.write_all(s.as_bytes()).await;
        }
    }
}

fn now_iso() -> String {
    // Cheap UTC timestamp without pulling chrono: seconds since epoch is enough for a log.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
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
    let _ = tokio::fs::write(&ep_path, serde_json::to_vec(&ep).unwrap_or_default()).await;
    restrict_to_user(&ep_path).await; // the token is a secret: user-only on Linux
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

/// chmod 600 on Unix so the bearer token is not world-readable; a no-op on Windows
/// (ACLs inherit from the user's data dir).
async fn restrict_to_user(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The endpoint the stdio shim connects to (written by `start`).
pub fn read_endpoint(data_dir: &std::path::Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(data_dir.join("mcp-endpoint.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((v.get("port")?.as_u64()? as u16, v.get("token")?.as_str()?.to_string()))
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
        assert_eq!(gate(ToolKind::Exec, Tier::ExecApprove), Decision::NeedApproval);
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
        assert!(mcp.try_busy("s2").is_some(), "a different session is independent");
        drop(g1);
        assert!(mcp.try_busy("s1").is_some(), "lock frees on drop");
    }

    #[test]
    fn token_is_hex_and_long() {
        let t = gen_token();
        assert_eq!(t.len(), 48);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
