//! MCP tool definitions and dispatch. Each call resolves a tier, passes the gate
//! (possibly via an in-app approval), runs, and is logged.

use super::rpc::{tool_error, tool_text};
use super::{
    gate, log_call, Decision, McpShared, Tier, ToolKind, APPROVAL_TIMEOUT, BRIDGE_TIMEOUT,
};
use crate::AppState;
use serde_json::{json, Value};
use tauri::Emitter;

/// The `tools/list` payload.
pub fn tool_specs() -> Value {
    let s = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    json!([
        { "name": "list_sessions", "description": "List open terminal sessions (id, title, kind ssh/local/serial, host, shell, cwd, permission tier, broadcast: source/target/off = its part in the app's broadcast input). A local session's host is this machine's name.",
          "inputSchema": s(json!({}), json!([])) },
        { "name": "list_connections", "description": "List the saved connection tree (folders and hosts), without credentials.",
          "inputSchema": s(json!({}), json!([])) },
        { "name": "get_connection", "description": "Effective settings of a saved node (inherited), with the credential removed.",
          "inputSchema": s(json!({ "node_id": { "type": "string" } }), json!(["node_id"])) },
        { "name": "read_scrollback", "description": "Recent commands (with exit codes and output) or lines from a session's scrollback, using OSC 133 marks.",
          "inputSchema": s(json!({ "session": { "type": "string" }, "last_n_commands": { "type": "integer" }, "last_n_lines": { "type": "integer" } }), json!(["session"])) },
        { "name": "run_command", "description": "Run a command in a session (SSH, or a local bash/zsh/PowerShell tab) and return its output and exit code. Runs only at an idle prompt. May require in-app approval. Give `sessions` (a list of session ids) instead of `session` to run the same command on each: every session's own tier gates it, one approval dialog lists all targets, and the result has one entry per session in the order given. A command starting with `sudo ` runs the rest as root on a separate channel (no terminal; the stored password goes to sudo, never to the command) when the session's tier allows root (approval with \"may run as root\", or mcp_sudo at trusted); sudo options, and sudo/su anywhere else in the command, are refused.",
          "inputSchema": s(json!({ "session": { "type": "string" }, "sessions": { "type": "array", "items": { "type": "string" } }, "command": { "type": "string" }, "timeout_s": { "type": "integer" } }), json!(["command"])) },
        { "name": "sftp_get", "description": "Copy a file from the session's filesystem (SFTP; the local disk for a local session) to a local path.",
          "inputSchema": s(json!({ "session": { "type": "string" }, "remote": { "type": "string" }, "local": { "type": "string" } }), json!(["session", "remote", "local"])) },
        { "name": "sftp_put", "description": "Copy a local file to a path on the session's filesystem (SFTP; the local disk for a local session). May require in-app approval.",
          "inputSchema": s(json!({ "session": { "type": "string" }, "local": { "type": "string" }, "remote": { "type": "string" } }), json!(["session", "local", "remote"])) },
        { "name": "open_in_editor", "description": "Open a file in the app's editor (local path, or a session + remote path). as_root opens it over sudo-sftp: gated like run_command plus the sudo opt-in.",
          "inputSchema": s(json!({ "path": { "type": "string" }, "session": { "type": "string" }, "remote_path": { "type": "string" }, "as_root": { "type": "boolean" } }), json!([])) },
    ])
}

fn kind_of(tool: &str, args: &Value) -> ToolKind {
    match tool {
        "run_command" | "sftp_put" => ToolKind::Exec,
        // Opening a file as root is as privileged as running a command.
        "open_in_editor" if as_root(args) => ToolKind::Exec,
        "sftp_get" | "open_in_editor" => ToolKind::File,
        _ => ToolKind::Read,
    }
}

fn as_root(args: &Value) -> bool {
    args.get("as_root")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing string argument {key:?}"))
}

/// Effective tier for a call: the session's node tier, else the profile default
/// (`mcp.default_tier`; for local shells, see `local_tier`).
async fn tier_for(state: &AppState, session: Option<&str>) -> Tier {
    let (default, local_default) = {
        let p = state.profile.read().unwrap();
        (
            Tier::parse(&p.mcp.default_tier),
            Tier::parse(&p.local.mcp_tier),
        )
    };
    let Some(id) = session else { return default };
    let Some(info) = state.sessions.info(id).await else {
        return default;
    };
    let node_tier = match &info.node_id {
        Some(n) => state
            .store
            .lock()
            .await
            .effective(n)
            .ok()
            .and_then(|s| s.mcp_tier)
            .map(|t| Tier::parse(&t)),
        None => None,
    };
    if info.kind == "local" {
        return local_tier(node_tier, info.admin, local_default);
    }
    node_tier.unwrap_or(default)
}

/// A local shell's tier: its saved node's `mcp_tier` when it sets one; else
/// `local.mcp_tier` (default read); but an elevated shell (Local (admin)) is read
/// unless its node says otherwise. Never trusted by default.
pub fn local_tier(node_tier: Option<Tier>, admin: bool, local_default: Tier) -> Tier {
    match node_tier {
        Some(t) => t,
        None if admin => Tier::Read,
        None => local_default,
    }
}

/// Ask the frontend something and wait for a reply (read_scrollback, open_in_editor).
async fn ask_frontend(
    state: &AppState,
    mcp: &McpShared,
    event: &str,
    mut payload: Value,
) -> Result<Value, String> {
    let id = uuid::Uuid::new_v4().to_string();
    payload["req_id"] = json!(id);
    let rx = mcp.expect(&id).await;
    state
        .sessions
        .app()
        .emit(event, &payload)
        .map_err(|e| e.to_string())?;
    crate::session::wake_ui();
    match tokio::time::timeout(BRIDGE_TIMEOUT, rx).await {
        Ok(Ok(v)) => Ok(v),
        _ => {
            mcp.cancel(&id).await;
            Err("no response from the app UI".into())
        }
    }
}

/// Pop an approval dialog for an exec tool. Returns `(allowed, allow_sudo)` where
/// `allow_sudo` is the dialog's separate "allow sudo password" opt-in (exec-approve
/// only; a remembered "always" never carries sudo).
async fn approved(
    state: &AppState,
    mcp: &McpShared,
    tool: &str,
    session: &str,
    command: &str,
) -> (bool, bool) {
    if mcp.always_allows(session).await {
        return (true, false);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let rx = mcp.expect(&id).await;
    let _ = state.sessions.app().emit(
        "mcp:approve",
        json!({ "req_id": id, "tool": tool, "session": session, "command": command }),
    );
    crate::session::wake_ui();
    let reply = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
        Ok(Ok(v)) => v,
        _ => {
            mcp.cancel(&id).await;
            json!({ "decision": "deny" })
        }
    };
    let decision = reply
        .get("decision")
        .and_then(|d| d.as_str())
        .unwrap_or("deny");
    let allow_sudo = reply
        .get("allow_sudo")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    if decision == "always" {
        mcp.remember_always(session).await;
    }
    let ok = decision == "approve" || decision == "always";
    (ok, ok && allow_sudo)
}

/// Whether MCP run_command may auto-answer the login user's sudo prompt:
/// never at read, the dialog's opt-in at exec-approve, and the node's
/// `Settings.mcp_sudo` at trusted.
pub fn sudo_allowed(tier: Tier, approval_sudo: bool, node_mcp_sudo: bool) -> bool {
    match tier {
        Tier::Read => false,
        Tier::ExecApprove => approval_sudo,
        Tier::Trusted => node_mcp_sudo,
    }
}

/// The session node's effective `mcp_sudo` opt-in (trusted tier), else false.
async fn node_mcp_sudo(state: &AppState, session: Option<&str>) -> bool {
    let Some(id) = session else { return false };
    let Some(info) = state.sessions.info(id).await else {
        return false;
    };
    let Some(node) = info.node_id else {
        return false;
    };
    state
        .store
        .lock()
        .await
        .effective(&node)
        .ok()
        .and_then(|s| s.mcp_sudo)
        .unwrap_or(false)
}

/// What a call asks for, for the log: the command line for `run_command`, the
/// path(s) for transfers and editor opens; nothing for the read-only listings.
pub fn subject(tool: &str, args: &Value) -> Option<String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str());
    match tool {
        "run_command" => s("command").map(str::to_string),
        "sftp_get" => Some(format!("{} -> {}", s("remote")?, s("local").unwrap_or("?"))),
        "sftp_put" => Some(format!("{} -> {}", s("local")?, s("remote").unwrap_or("?"))),
        "open_in_editor" => {
            let p = s("remote_path").or_else(|| s("path"))?;
            Some(if as_root(args) {
                format!("as root: {p}")
            } else {
                p.to_string()
            })
        }
        "get_connection" => s("node_id").map(str::to_string),
        _ => None,
    }
}

/// run_command's targets when it is given `sessions`: `session` (if any) first,
/// then the list, in order, without repeats or blanks. `None` = a single-session call.
pub fn multi_targets(args: &Value) -> Option<Vec<String>> {
    let list = args.get("sessions")?.as_array()?;
    let mut out: Vec<String> = Vec::new();
    let first = args.get("session").and_then(|v| v.as_str());
    for id in first
        .into_iter()
        .chain(list.iter().filter_map(|v| v.as_str()))
    {
        let id = id.trim();
        if !id.is_empty() && !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// One target of a multi-session run_command: its own tier's gate, where an
/// "always for this session" answer counts as approval already given.
pub fn target_decision(tier: Tier, always: bool) -> Decision {
    match gate(ToolKind::Exec, tier) {
        Decision::NeedApproval if always => Decision::Allow,
        d => d,
    }
}

/// Run one tool call. Returns the MCP tool result value (never a transport error;
/// tool failures are surfaced as `isError` content).
pub async fn call(state: &AppState, mcp: &McpShared, name: &str, args: Value) -> Value {
    if name == "run_command" {
        if let Some(targets) = multi_targets(&args) {
            return call_multi(state, mcp, &args, targets).await;
        }
    }
    let session = args
        .get("session")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let tier = tier_for(state, session.as_deref()).await;
    let kind = kind_of(name, &args);

    let started = std::time::Instant::now();
    let subj = subject(name, &args);
    let subj = subj.as_deref();
    // Gate. Approval (if any) happens BEFORE run(), so a denied/timed-out command
    // is never sent to the shell.
    let mut approval_sudo = false;
    let mut node_sudo = false;
    match gate(kind, tier) {
        Decision::Deny => {
            let why = format!(
                "permission denied: tier '{}' does not allow {name}",
                tier.as_str()
            );
            log_call(
                state,
                name,
                session.as_deref(),
                tier,
                "deny",
                false,
                None,
                started.elapsed().as_millis(),
                subj,
                Some(&why),
            )
            .await;
            return tool_error(&why);
        }
        Decision::NeedApproval => {
            // The approval dialog shows the command; the log records it as `subject`.
            let root_open = (name == "open_in_editor").then(|| {
                format!(
                    "open as root: {}",
                    args.get("remote_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                )
            });
            let cmd = root_open
                .as_deref()
                .or_else(|| {
                    args.get("command")
                        .or_else(|| args.get("local"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or(name);
            let sess = session.clone().unwrap_or_default();
            let (ok, sudo) = approved(state, mcp, name, &sess, cmd).await;
            if !ok {
                log_call(
                    state,
                    name,
                    session.as_deref(),
                    tier,
                    "denied",
                    false,
                    None,
                    started.elapsed().as_millis(),
                    subj,
                    None,
                )
                .await;
                return tool_error(
                    "denied by the user (or approval timed out — the command was not run)",
                );
            }
            approval_sudo = sudo;
        }
        Decision::Allow => {
            if tier == Tier::Trusted {
                node_sudo = node_mcp_sudo(state, session.as_deref()).await;
            }
        }
    }
    let allow_sudo = sudo_allowed(tier, approval_sudo, node_sudo);

    let decision = if gate(kind, tier) == Decision::NeedApproval {
        "approved"
    } else {
        "allow"
    };
    let result = run(state, mcp, name, &args, session.as_deref(), allow_sudo).await;
    let (ok, exit) = match &result {
        Ok(v) => (
            true,
            v.get("exit_code")
                .and_then(|c| c.as_i64())
                .map(|c| c as i32),
        ),
        Err(_) => (false, None),
    };
    let error = result.as_ref().err().map(String::as_str);
    log_call(
        state,
        name,
        session.as_deref(),
        tier,
        decision,
        ok,
        exit,
        started.elapsed().as_millis(),
        subj,
        error,
    )
    .await;
    match result {
        Ok(v) => tool_text(&v),
        Err(e) => tool_error(&e),
    }
}

async fn run(
    state: &AppState,
    mcp: &McpShared,
    name: &str,
    args: &Value,
    session: Option<&str>,
    allow_sudo: bool,
) -> Result<Value, String> {
    match name {
        "list_sessions" => {
            let mut out = Vec::new();
            for info in state.sessions.list().await {
                let tier = tier_for(state, Some(&info.id)).await;
                out.push(json!({
                    "id": info.id, "title": info.title, "kind": info.kind,
                    "host": info.host, "shell": info.shell, "cwd": info.cwd,
                    "tier": tier.as_str(),
                    "broadcast": state.broadcast.role(&info.id),
                }));
            }
            Ok(json!(out))
        }
        "list_connections" => {
            let nodes = state.store.lock().await.all().map_err(|e| e.to_string())?;
            let out: Vec<Value> = nodes
                .iter()
                .map(|n| json!({ "id": n.id, "parent_id": n.parent_id, "kind": n.kind, "name": n.name,
                                 "host": n.settings.host, "user": n.settings.user, "tags": n.settings.tags }))
                .collect();
            Ok(json!(out))
        }
        "get_connection" => {
            let node_id = arg_str(args, "node_id")?;
            let mut eff = state
                .store
                .lock()
                .await
                .effective(node_id)
                .map_err(|e| e.to_string())?;
            eff.credential = None; // never expose the credential reference
            serde_json::to_value(eff).map_err(|e| e.to_string())
        }
        "read_scrollback" => {
            let session = session.ok_or("session is required")?;
            let payload = json!({
                "session": session,
                "last_n_commands": args.get("last_n_commands"),
                "last_n_lines": args.get("last_n_lines"),
            });
            ask_frontend(state, mcp, "mcp:scrollback", payload).await
        }
        "run_command" => {
            let session = session.ok_or("session (or sessions) is required")?;
            run_command(state, mcp, session, args, allow_sudo).await
        }
        "sftp_get" => {
            let session = session.ok_or("session is required")?;
            let (remote, local) = (arg_str(args, "remote")?, arg_str(args, "local")?);
            let fs = state
                .sessions
                .get(session)
                .await
                .map_err(|e| e.to_string())?
                .fs()
                .await
                .map_err(|e| e.to_string())?;
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let t = crate::sftp::Transfer::new(uuid::Uuid::new_v4().to_string(), None, flag);
            fs.download(remote, std::path::Path::new(local), &t)
                .await
                .map_err(|e| format!("{e:#}"))?;
            Ok(json!({ "ok": true, "local": local }))
        }
        "sftp_put" => {
            let session = session.ok_or("session is required")?;
            let (local, remote) = (arg_str(args, "local")?, arg_str(args, "remote")?);
            let fs = state
                .sessions
                .get(session)
                .await
                .map_err(|e| e.to_string())?
                .fs()
                .await
                .map_err(|e| e.to_string())?;
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let t = crate::sftp::Transfer::new(uuid::Uuid::new_v4().to_string(), None, flag);
            fs.upload(std::path::Path::new(local), remote, &t)
                .await
                .map_err(|e| format!("{e:#}"))?;
            Ok(json!({ "ok": true, "remote": remote }))
        }
        "open_in_editor" => {
            // Root needs the same sudo opt-in as run_command's sudo auto-answer, and a
            // remote path: there is no sudo backend for the local disk.
            let root = as_root(args);
            if root && !allow_sudo {
                return Err("opening as root needs the sudo opt-in (\"allow sudo\" in the approval dialog, or the node's mcp_sudo at trusted)".into());
            }
            if root && (session.is_none() || args.get("remote_path").is_none()) {
                return Err("as_root needs session + remote_path".into());
            }
            let payload = json!({
                "path": args.get("path"),
                "session": args.get("session"),
                "remote_path": args.get("remote_path"),
                "as_root": root,
            });
            ask_frontend(state, mcp, "mcp:open_editor", payload).await
        }
        other => Err(format!("unknown tool {other:?}")),
    }
}

/// run_command on one session: `{output, exit_code}`.
async fn run_command(
    state: &AppState,
    mcp: &McpShared,
    session: &str,
    args: &Value,
    allow_sudo: bool,
) -> Result<Value, String> {
    let command = arg_str(args, "command")?;
    // One MCP command per session at a time (never interleave hidden commands).
    let _guard = mcp
        .try_busy(session)
        .ok_or("another MCP command is already running on this session")?;
    // Don't clobber what the user is typing in that tab.
    if state
        .sessions
        .typed_recently(session, std::time::Duration::from_secs(3))
        .await
    {
        return Err("the user is typing in this session; try again in a moment".into());
    }
    let sess = state
        .sessions
        .get(session)
        .await
        .map_err(|e| e.to_string())?;
    let timeout = args
        .get("timeout_s")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 600);
    // `sudo …` never goes through the terminal: Charon runs the rest as root on
    // its own channel and gives the password to sudo's stdin only, so a prompt
    // printed in the output (real or fake) is never answered. Needs the opt-in
    // (approval dialog "may run as root", or the node's mcp_sudo at trusted).
    let root = crate::hidden::root_command(command)?;
    let limit = std::time::Duration::from_secs(timeout);
    let res = match &root {
        Some(script) => {
            if !allow_sudo {
                return Err("this command runs as root: approve it with \"may run as root\" ticked (or set mcp_sudo on a trusted node)".into());
            }
            let pw = zeroize::Zeroizing::new(sess.ssh_password().await);
            tokio::time::timeout(limit, sess.exec_root(script, pw.as_deref())).await
        }
        None => tokio::time::timeout(limit, sess.exec_hidden(command)).await,
    };
    match res {
        Ok(Ok((out, code))) => Ok(json!({
            "output": crate::hidden::strip_escapes(&String::from_utf8_lossy(&out)),
            "exit_code": code,
            "as_root": root.is_some(),
        })),
        Ok(Err(e)) if root.is_some() => Err(format!("{e:#}")),
        Ok(Err(e)) => Err(format!("{e:#} (does this session have shell integration?)")),
        Err(_) => Err(format!("command timed out after {timeout}s")),
    }
}

/// One session of a multi-session run_command, as gated.
struct Target {
    id: String,
    tier: Tier,
    decision: Decision,
    /// The tier asks for approval (even if "always" already gave it): logged as "approved".
    gated: bool,
}

/// Merge `extra`'s fields into the object `v`.
fn with(mut v: Value, extra: Value) -> Value {
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
    v
}

/// run_command with `sessions`: each target is gated by its own tier; the ones
/// that need approval share one dialog listing every target (and what each will
/// get); the allowed ones run concurrently. One log line per target; the result
/// lists the targets in the order given.
async fn call_multi(state: &AppState, mcp: &McpShared, args: &Value, ids: Vec<String>) -> Value {
    let command = match arg_str(args, "command") {
        Ok(c) => c.to_string(),
        Err(e) => return tool_error(&e),
    };
    let subj = subject("run_command", args);
    let mut targets = Vec::new();
    let mut listed = Vec::new();
    for id in ids {
        let tier = tier_for(state, Some(&id)).await;
        let decision = target_decision(tier, mcp.always_allows(&id).await);
        let info = state.sessions.info(&id).await;
        listed.push(json!({
            "session": id,
            "title": info.as_ref().map(|i| i.title.clone()),
            "host": info.as_ref().and_then(|i| i.host.clone()),
            "tier": tier.as_str(),
            "decision": match decision { Decision::Allow => "allow", Decision::Deny => "deny", Decision::NeedApproval => "approve" },
        }));
        targets.push(Target {
            gated: gate(ToolKind::Exec, tier) == Decision::NeedApproval,
            id,
            tier,
            decision,
        });
    }
    // One dialog for every target that needs it; nothing runs before it is answered.
    let need: Vec<&str> = targets
        .iter()
        .filter(|t| t.decision == Decision::NeedApproval)
        .map(|t| t.id.as_str())
        .collect();
    let (mut approved_all, mut approval_sudo) = (false, false);
    if !need.is_empty() {
        let id = uuid::Uuid::new_v4().to_string();
        let rx = mcp.expect(&id).await;
        let _ = state.sessions.app().emit(
            "mcp:approve",
            json!({ "req_id": id, "tool": "run_command", "session": need[0], "command": command, "targets": listed }),
        );
        crate::session::wake_ui();
        let reply = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(v)) => v,
            _ => {
                mcp.cancel(&id).await;
                json!({ "decision": "deny" })
            }
        };
        let decision = reply
            .get("decision")
            .and_then(|d| d.as_str())
            .unwrap_or("deny");
        if decision == "always" {
            for s in &need {
                mcp.remember_always(s).await;
            }
        }
        approved_all = decision == "approve" || decision == "always";
        approval_sudo = approved_all
            && reply
                .get("allow_sudo")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
    }
    let subj = subj.as_deref();
    let runs = targets.iter().map(|t| async move {
        let t0 = std::time::Instant::now();
        let base = json!({ "session": t.id, "tier": t.tier.as_str() });
        match t.decision {
            Decision::Deny => {
                let why = format!("permission denied: tier '{}' does not allow run_command", t.tier.as_str());
                log_call(state, "run_command", Some(&t.id), t.tier, "deny", false, None, t0.elapsed().as_millis(), subj, Some(&why)).await;
                return with(base, json!({ "ok": false, "error": why }));
            }
            Decision::NeedApproval if !approved_all => {
                log_call(state, "run_command", Some(&t.id), t.tier, "denied", false, None, t0.elapsed().as_millis(), subj, None).await;
                return with(base, json!({ "ok": false, "error": "denied by the user (or approval timed out — the command was not run)" }));
            }
            _ => {}
        }
        // The dialog's sudo opt-in covers only the targets it asked about.
        let node_sudo = t.tier == Tier::Trusted && node_mcp_sudo(state, Some(&t.id)).await;
        let allow_sudo = sudo_allowed(t.tier, approval_sudo && t.decision == Decision::NeedApproval, node_sudo);
        let r = run_command(state, mcp, &t.id, args, allow_sudo).await;
        let decision = if t.gated { "approved" } else { "allow" };
        let exit = r.as_ref().ok().and_then(|v| v.get("exit_code")).and_then(|c| c.as_i64()).map(|c| c as i32);
        log_call(state, "run_command", Some(&t.id), t.tier, decision, r.is_ok(), exit, t0.elapsed().as_millis(), subj, r.as_ref().err().map(String::as_str)).await;
        match r {
            Ok(v) => with(with(base, json!({ "ok": true })), v),
            Err(e) => with(base, json!({ "ok": false, "error": e })),
        }
    });
    let results = futures::future::join_all(runs).await;
    tool_text(&json!({ "command": command, "results": results }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_targets_keep_order_without_repeats() {
        assert_eq!(
            multi_targets(&json!({ "session": "a", "command": "x" })),
            None
        );
        assert_eq!(
            multi_targets(&json!({ "sessions": [], "command": "x" })),
            None
        );
        let t = multi_targets(&json!({ "sessions": ["c", "a", "b", "a", " ", 7], "command": "x" }))
            .unwrap();
        assert_eq!(t, ["c", "a", "b"]);
        // `session` alongside `sessions` goes first.
        let t = multi_targets(&json!({ "session": "b", "sessions": ["c", "b"], "command": "x" }))
            .unwrap();
        assert_eq!(t, ["b", "c"]);
    }

    #[test]
    fn each_target_is_gated_by_its_own_tier() {
        let tiers = [
            Tier::Read,
            Tier::ExecApprove,
            Tier::ExecApprove,
            Tier::Trusted,
        ];
        let always = [false, false, true, false];
        let got: Vec<Decision> = tiers
            .iter()
            .zip(always)
            .map(|(t, a)| target_decision(*t, a))
            .collect();
        // read: refused, never prompted for; exec-approve: asked; already "always": runs; trusted: runs.
        assert_eq!(
            got,
            [
                Decision::Deny,
                Decision::NeedApproval,
                Decision::Allow,
                Decision::Allow
            ]
        );
        // "always" never lifts a read tier.
        assert_eq!(target_decision(Tier::Read, true), Decision::Deny);
    }

    #[test]
    fn merged_result_fields() {
        let v = with(
            with(
                json!({ "session": "s", "tier": "trusted" }),
                json!({ "ok": true }),
            ),
            json!({ "output": "x", "exit_code": 0 }),
        );
        assert_eq!(
            v,
            json!({ "session": "s", "tier": "trusted", "ok": true, "output": "x", "exit_code": 0 })
        );
    }

    #[test]
    fn log_subject_is_what_the_agent_asked_for() {
        let cmd =
            serde_json::json!({ "session": "s", "command": "grep -n 'CHARON' docker-compose.yml" });
        assert_eq!(
            subject("run_command", &cmd).as_deref(),
            Some("grep -n 'CHARON' docker-compose.yml")
        );
        let put =
            serde_json::json!({ "session": "s", "local": "C:/x.txt", "remote": "/tmp/x.txt" });
        assert_eq!(
            subject("sftp_put", &put).as_deref(),
            Some("C:/x.txt -> /tmp/x.txt")
        );
        let root =
            serde_json::json!({ "session": "s", "remote_path": "/etc/hosts", "as_root": true });
        assert_eq!(
            subject("open_in_editor", &root).as_deref(),
            Some("as root: /etc/hosts")
        );
        assert_eq!(subject("list_sessions", &serde_json::json!({})), None);
    }

    #[test]
    fn local_tier_defaults() {
        // Bare Local: the profile's local.mcp_tier.
        assert_eq!(local_tier(None, false, Tier::Read), Tier::Read);
        assert_eq!(
            local_tier(None, false, Tier::ExecApprove),
            Tier::ExecApprove
        );
        // Local (admin): read, whatever local.mcp_tier says.
        assert_eq!(local_tier(None, true, Tier::Trusted), Tier::Read);
        assert_eq!(local_tier(None, true, Tier::ExecApprove), Tier::Read);
        // A saved node's own tier wins, elevated or not.
        assert_eq!(
            local_tier(Some(Tier::ExecApprove), true, Tier::Read),
            Tier::ExecApprove
        );
        assert_eq!(
            local_tier(Some(Tier::Read), false, Tier::Trusted),
            Tier::Read
        );
    }

    #[test]
    fn sudo_is_tier_gated() {
        // read: never, whatever the inputs.
        assert!(!sudo_allowed(Tier::Read, true, true));
        // exec-approve: only the approval dialog's opt-in decides.
        assert!(sudo_allowed(Tier::ExecApprove, true, false));
        assert!(!sudo_allowed(Tier::ExecApprove, false, true));
        // trusted: only the node's Settings.mcp_sudo decides.
        assert!(sudo_allowed(Tier::Trusted, false, true));
        assert!(!sudo_allowed(Tier::Trusted, true, false));
    }

    #[test]
    fn open_in_editor_as_root_is_gated_like_exec() {
        let plain = json!({ "session": "s", "remote_path": "/etc/hosts" });
        let root = json!({ "session": "s", "remote_path": "/etc/hosts", "as_root": true });
        assert_eq!(kind_of("open_in_editor", &plain), ToolKind::File);
        assert_eq!(kind_of("open_in_editor", &root), ToolKind::Exec);
        // read: denied; exec-approve: needs the approval dialog; trusted: allowed.
        assert_eq!(
            gate(kind_of("open_in_editor", &root), Tier::Read),
            Decision::Deny
        );
        assert_eq!(
            gate(kind_of("open_in_editor", &root), Tier::ExecApprove),
            Decision::NeedApproval
        );
        assert_eq!(
            gate(kind_of("open_in_editor", &root), Tier::Trusted),
            Decision::Allow
        );
        // A plain open at exec-approve stays prompt-free.
        assert_eq!(
            gate(kind_of("open_in_editor", &plain), Tier::ExecApprove),
            Decision::Allow
        );
    }
}
