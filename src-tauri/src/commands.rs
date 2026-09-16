//! Tauri commands = the UI's API. Keep these thin; logic lives in the other modules.

use crate::connections::Node;
use crate::local::{LocalParams, LocalSession};
use crate::serial::{PortInfo, SerialSession, SerialSettings};
use crate::session::SessionInfo;
use crate::ssh::{Auth, Hop, HostKeyChanged, SshError, SshParams, SshSession};
use crate::vault::{Secret, SecretKind, SecretMeta, VaultStatus};
use crate::AppState;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::State;

type CmdResult<T> = Result<T, String>;

/// `{:#}` so anyhow errors print their whole chain ("connect h:22: host key
/// rejected"), not just the outermost context.
fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub portable: bool,
    pub data_dir: String,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        portable: state.paths.portable,
        data_dir: state.paths.data_dir.display().to_string(),
    }
}

#[tauri::command]
pub async fn list_connections(state: State<'_, AppState>) -> CmdResult<Vec<Node>> {
    state.store.lock().await.all().map_err(err)
}

#[tauri::command]
pub async fn save_connection(state: State<'_, AppState>, node: Node) -> CmdResult<()> {
    state.store.lock().await.upsert(&node).map_err(err)
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.store.lock().await.delete(&id).map_err(err)
}

// ---------------------------------------------------------------- profile

#[tauri::command]
pub fn get_profile(state: State<'_, AppState>) -> crate::profile::Profile {
    state.profile.read().unwrap().clone()
}

/// Full replace + write to data/profiles/<name>.toml.
#[tauri::command]
pub fn save_profile(state: State<'_, AppState>, profile: crate::profile::Profile) -> CmdResult<()> {
    crate::profile::save(&state.paths.data_dir, &profile).map_err(err)?;
    *state.profile.write().unwrap() = profile;
    Ok(())
}

// ---------------------------------------------------------------- import

/// `kind`: "ssh_config" | "putty" | "mobaxterm"; `path` overrides the default location.
#[tauri::command]
pub async fn import_connections(
    state: State<'_, AppState>,
    kind: String,
    path: Option<String>,
    policy: Option<crate::import::DupPolicy>,
) -> CmdResult<crate::import::ImportReport> {
    let source = crate::import::Source::parse_kind(&kind).map_err(err)?;
    let path = path.filter(|p| !p.trim().is_empty()).map(std::path::PathBuf::from);
    let parsed = crate::import::load(source, path.as_deref()).map_err(err)?;
    let store = state.store.lock().await;
    crate::import::apply(&store, state.vault.as_ref(), source, parsed, policy.unwrap_or_default()).map_err(err)
}

/// Parse only: what would be added and what already exists. Parsing is cheap,
/// so `import_connections` simply parses again when the user confirms.
#[tauri::command]
pub async fn import_preview(
    state: State<'_, AppState>,
    kind: String,
    path: Option<String>,
) -> CmdResult<crate::import::ImportPreview> {
    let source = crate::import::Source::parse_kind(&kind).map_err(err)?;
    let path = path.filter(|p| !p.trim().is_empty()).map(std::path::PathBuf::from);
    let parsed = crate::import::load(source, path.as_deref()).map_err(err)?;
    let store = state.store.lock().await;
    crate::import::preview(&store, &parsed).map_err(err)
}

// ---------------------------------------------------------------- vault
// Secret values never cross this boundary outward; the UI only sees names + kinds.

#[tauri::command]
pub fn vault_status(state: State<'_, AppState>) -> VaultStatus {
    state.vault.status()
}

/// Sync on purpose: scrypt takes ~1 s and Tauri runs sync commands off the async runtime.
#[tauri::command]
pub fn vault_unlock(state: State<'_, AppState>, master: String) -> CmdResult<VaultStatus> {
    state.vault.unlock(&master).map_err(err)?;
    Ok(state.vault.status())
}

#[tauri::command]
pub fn vault_list(state: State<'_, AppState>) -> CmdResult<Vec<SecretMeta>> {
    state.vault.list().map_err(err)
}

#[tauri::command]
pub fn vault_set(state: State<'_, AppState>, name: String, secret: Secret) -> CmdResult<()> {
    state.vault.set(&name, &secret).map_err(err)
}

#[tauri::command]
pub fn vault_delete(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    state.vault.delete(&name).map_err(err)
}

// ---------------------------------------------------------------- sessions

/// `session_open_ssh` needs one distinguishable failure: "nothing to authenticate
/// with", so the UI can ask for a password inline instead of showing an error.
#[derive(Serialize)]
pub struct OpenError {
    /// "need_credential" | "hostkey_changed" | "error"
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostkey: Option<HostKeyChanged>,
}

impl OpenError {
    fn other<E: std::fmt::Display>(e: E) -> Self {
        Self { kind: "error", message: format!("{e:#}"), hostkey: None }
    }

    /// Classify a connect failure: host key mismatches get their own kind + details.
    fn connect(e: anyhow::Error) -> Self {
        match e.downcast_ref::<SshError>() {
            Some(SshError::HostKeyChanged(d)) => {
                Self { kind: "hostkey_changed", message: format!("{e:#}"), hostkey: Some(d.clone()) }
            }
            _ => Self::other(e),
        }
    }
}

#[derive(Deserialize)]
pub struct OpenSsh {
    /// Session id chosen by the UI so it can subscribe to `ssh:hostkey:{id}`
    /// before the connection starts. Generated here if absent.
    pub id: Option<String>,
    /// Either a saved node id…
    pub node_id: Option<String>,
    /// …or ad-hoc quick-connect fields.
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub key_path: Option<String>,
    /// Vault entry name; overrides the node's effective credential.
    pub credential: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[tauri::command]
pub async fn session_open_ssh(state: State<'_, AppState>, req: OpenSsh) -> Result<SessionInfo, OpenError> {
    // Resolve saved node → effective settings, quick-connect fields override.
    let eff = match &req.node_id {
        Some(id) => state.store.lock().await.effective(id).map_err(OpenError::other)?,
        None => Default::default(),
    };
    let host = req.host.or(eff.host).ok_or_else(|| OpenError::other("host required"))?;
    let user = req.user.or(eff.user).unwrap_or_else(whoami);
    let port = req.port.or(eff.port).unwrap_or(22);

    // Inline password/key wins; otherwise the credential name resolves through the vault.
    let auth = if let Some(key) = req.key_path {
        Auth::Key { key, passphrase: req.password }
    } else if let Some(pw) = req.password {
        Auth::Password(pw)
    } else if let Some(name) = req.credential.or(eff.credential) {
        match state.vault.get(&name).map_err(OpenError::other)? {
            Some(Secret { kind: SecretKind::Password, value, .. }) => Auth::Password(value),
            Some(Secret { kind: SecretKind::Key, value, key_passphrase }) => {
                Auth::Key { key: value, passphrase: key_passphrase }
            }
            None => {
                return Err(OpenError {
                    kind: "need_credential",
                    message: format!("credential {name:?} is not in the vault"),
                    hostkey: None,
                })
            }
        }
    } else {
        return Err(OpenError { kind: "need_credential", message: "no credential".into(), hostkey: None });
    };

    // Jump hosts: `jump_host` names a saved node (which may itself have a jump
    // host) or is a plain `[user@]host[:port]`. Hops without their own
    // credential reuse the target's.
    let mut jumps = Vec::new();
    if let Some(j) = eff.jump_host.as_deref().filter(|j| !j.trim().is_empty()) {
        let store = state.store.lock().await;
        resolve_jumps(&store, state.vault.as_ref(), j, &user, &auth, &mut jumps, 0)?;
    }

    let id = req.id.filter(|s| !s.is_empty()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let params = SshParams { host: host.clone(), port, user: user.clone(), auth, jumps, cols: req.cols, rows: req.rows };

    let sess = SshSession::connect(state.sessions.clone(), state.hostkeys.clone(), id.clone(), params)
        .await
        .map_err(OpenError::connect)?;
    let mut info = SessionInfo::new(id, format!("{user}@{host}"), "ssh");
    info.shell_integration = eff.shell_integration.unwrap_or(true);
    info.node_id = req.node_id.clone();
    info.host = Some(host.clone());
    state.sessions.register(info.clone(), sess).await;
    Ok(info)
}

#[derive(Deserialize)]
pub struct OpenLocal {
    pub id: Option<String>,
    pub node_id: Option<String>,
    pub shell: Option<String>,
    pub cwd: Option<String>,
    /// Elevated shell (Windows sudo inline / `sudo -i`); see `local::elevation_for`.
    #[serde(default)]
    pub admin: bool,
    pub cols: u16,
    pub rows: u16,
}

#[tauri::command]
pub async fn session_open_local(state: State<'_, AppState>, req: OpenLocal) -> CmdResult<SessionInfo> {
    let eff = match &req.node_id {
        Some(id) => state.store.lock().await.effective(id).map_err(err)?,
        None => Default::default(),
    };
    let id = req.id.filter(|s| !s.is_empty()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let shell = req.shell.or(eff.shell).filter(|s| !s.trim().is_empty()).unwrap_or_else(crate::local::default_shell);
    let (program, args) = if req.admin {
        match crate::local::elevation_for(&shell) {
            crate::local::Elevation::Inline { program, args } => (program, args),
            crate::local::Elevation::NewWindow { why } => {
                // Best we can do: an elevated console of its own, and say why.
                #[cfg(windows)]
                crate::local::open_elevated_window(&shell).map_err(err)?;
                return Err(format!(
                    "{why}. An elevated ConPTY cannot be embedded from a non-elevated process, so the shell was opened in its own window instead."
                ));
            }
        }
    } else {
        (shell.clone(), vec![])
    };
    let params = LocalParams { shell: Some(program), args, cwd: req.cwd.or(eff.cwd), cols: req.cols, rows: req.rows };
    let sess = LocalSession::spawn(state.sessions.app().clone(), id.clone(), params).map_err(err)?;
    let title = if req.admin { format!("{} (admin)", basename(&shell)) } else { sess.program.clone() };
    let mut info = SessionInfo::new(id, title, "local");
    info.node_id = req.node_id.clone();
    // Local shells get shell-integration too (only injected when a known shell is
    // detected from the prompt — pwsh/bash/zsh; cmd.exe is left alone).
    info.shell_integration = eff.shell_integration.unwrap_or(true);
    state.sessions.register(info.clone(), sess).await;
    Ok(info)
}

fn basename(p: &str) -> String {
    std::path::Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

#[derive(Deserialize)]
pub struct OpenSerial {
    pub id: Option<String>,
    /// Saved node, or…
    pub node_id: Option<String>,
    /// …ad-hoc line settings (override the node's if both given).
    pub serial: Option<SerialSettings>,
    pub cols: u16,
    pub rows: u16,
}

#[tauri::command]
pub async fn session_open_serial(state: State<'_, AppState>, req: OpenSerial) -> CmdResult<SessionInfo> {
    let eff = match &req.node_id {
        Some(id) => state.store.lock().await.effective(id).map_err(err)?,
        None => Default::default(),
    };
    let settings = req.serial.or(eff.serial).unwrap_or_default();
    let id = req.id.filter(|s| !s.is_empty()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let sess = SerialSession::open(state.sessions.app().clone(), id.clone(), &settings).map_err(err)?;
    let info = SessionInfo::new(id, settings.label(), "serial");
    state.sessions.register(info.clone(), sess).await;
    Ok(info)
}

#[tauri::command]
pub fn serial_list_ports() -> CmdResult<Vec<PortInfo>> {
    crate::serial::list_ports().map_err(err)
}

/// Turn a `jump_host` string into hops, outermost first. A saved node's own
/// jump host is followed recursively (bounded), so "web → bastion → outer" works.
fn resolve_jumps(
    store: &crate::connections::Store,
    vault: &dyn crate::vault::Vault,
    jump: &str,
    default_user: &str,
    default_auth: &Auth,
    out: &mut Vec<Hop>,
    depth: u8,
) -> Result<(), OpenError> {
    if depth > 4 {
        return Err(OpenError::other("jump host chain too deep (loop?)"));
    }
    let jump = jump.trim();
    // A saved node by name, or by exact host.
    let node = store
        .all()
        .map_err(OpenError::other)?
        .into_iter()
        .find(|n| n.kind == crate::connections::Kind::Ssh && (n.name == jump || n.settings.host.as_deref() == Some(jump)));
    let (host, port, user, credential, nested) = match node {
        Some(n) => {
            let e = store.effective(&n.id).map_err(OpenError::other)?;
            let host = e.host.ok_or_else(|| OpenError::other(format!("jump host {jump:?} has no address")))?;
            (host, e.port.unwrap_or(22), e.user, e.credential, e.jump_host)
        }
        None => {
            let (user, rest) = match jump.rsplit_once('@') {
                Some((u, r)) => (Some(u.to_string()), r),
                None => (None, jump),
            };
            let (host, port) = match rest.rsplit_once(':') {
                Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() => {
                    (h.to_string(), p.parse().map_err(|_| OpenError::other(format!("bad port in jump host {jump:?}")))?)
                }
                _ => (rest.to_string(), 22),
            };
            (host, port, user, None, None)
        }
    };
    // Outer hops first: recurse before pushing this one.
    if let Some(n) = nested.filter(|n| !n.trim().is_empty() && n.trim() != jump) {
        resolve_jumps(store, vault, &n, default_user, default_auth, out, depth + 1)?;
    }
    let auth = match credential {
        Some(name) => match vault.get(&name).map_err(OpenError::other)? {
            Some(Secret { kind: SecretKind::Password, value, .. }) => Auth::Password(value),
            Some(Secret { kind: SecretKind::Key, value, key_passphrase }) => Auth::Key { key: value, passphrase: key_passphrase },
            None => return Err(OpenError::other(format!("credential {name:?} for jump host {host} is not in the vault"))),
        },
        None => default_auth.clone(),
    };
    out.push(Hop { host, port, user: user.unwrap_or_else(|| default_user.to_string()), auth });
    Ok(())
}

/// "Host was reinstalled": drop the stale entry from Charon's own known_hosts.
/// The user's ~/.ssh/known_hosts is never modified.
#[tauri::command]
pub fn hostkey_forget(state: State<'_, AppState>, host: String, port: u16) -> CmdResult<usize> {
    state.hostkeys.forget(&host, port).map_err(err)
}

/// UI's reply to an `ssh:hostkey:{id}` prompt.
#[tauri::command]
pub async fn ssh_hostkey_answer(state: State<'_, AppState>, id: String, accept: bool) -> CmdResult<()> {
    state.sessions.answer_hostkey(&id, accept).await.map_err(err)
}

/// The UI detected the remote shell from its prompt; remember it on the session.
#[tauri::command]
pub async fn session_set_shell(state: State<'_, AppState>, id: String, shell: Option<String>) -> CmdResult<()> {
    state.sessions.set_shell(&id, shell).await.map_err(err)
}

/// The shell reported its cwd over OSC 7.
#[tauri::command]
pub async fn session_set_cwd(state: State<'_, AppState>, id: String, cwd: Option<String>) -> CmdResult<()> {
    state.sessions.set_cwd(&id, cwd).await.map_err(err)
}

#[tauri::command]
pub async fn session_write(state: State<'_, AppState>, id: String, data_b64: String) -> CmdResult<()> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64).map_err(err)?;
    state.sessions.mark_user_write(&id).await;
    state.sessions.get(&id).await.map_err(err)?.write(&bytes).await.map_err(err)
}

#[tauri::command]
pub async fn session_resize(state: State<'_, AppState>, id: String, cols: u16, rows: u16) -> CmdResult<()> {
    state.sessions.get(&id).await.map_err(err)?.resize(cols, rows).await.map_err(err)
}

#[tauri::command]
pub async fn session_close(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    if let Ok(s) = state.sessions.get(&id).await {
        let _ = s.close().await;
    }
    state.sessions.remove(&id).await;
    Ok(())
}

fn whoami() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "root".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::{Kind, Settings, Store};
    use crate::vault::{FileVault, Vault};
    use std::path::Path;

    fn node(id: &str, name: &str, parent: Option<&str>, settings: Settings) -> Node {
        Node { id: id.into(), parent_id: parent.map(str::to_string), kind: Kind::Ssh, name: name.into(), sort: 0, settings }
    }

    fn hops(store: &Store, vault: &dyn crate::vault::Vault, jump: &str) -> Vec<(String, u16, String, bool)> {
        let mut out = Vec::new();
        resolve_jumps(store, vault, jump, "deflt", &Auth::Password("pw".into()), &mut out, 0).map_err(|e| e.message).unwrap();
        out.into_iter()
            .map(|h| (h.host, h.port, h.user, matches!(h.auth, Auth::Password(ref p) if p == "pw")))
            .collect()
    }

    #[test]
    fn plain_jump_strings_parse_and_reuse_target_auth() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let vault = FileVault::open(tempfile::tempdir().unwrap().path());
        assert_eq!(hops(&store, &vault, "bastion"), vec![("bastion".into(), 22, "deflt".into(), true)]);
        assert_eq!(hops(&store, &vault, "ops@bastion:2222"), vec![("bastion".into(), 2222, "ops".into(), true)]);
        assert_eq!(hops(&store, &vault, "  ops@10.0.0.1 "), vec![("10.0.0.1".into(), 22, "ops".into(), true)]);
    }

    #[test]
    fn saved_node_chain_resolves_outermost_first_with_its_own_credential() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::open(dir.path());
        vault.unlock("m").unwrap();
        vault.set("bastion-key", &Secret { kind: SecretKind::Key, value: "/k".into(), key_passphrase: None }).unwrap();
        store.upsert(&node("outer", "outer", None, Settings { host: Some("outer.example".into()), user: Some("o".into()), ..Default::default() })).unwrap();
        store.upsert(&node("bastion", "bastion", None, Settings {
            host: Some("b.example".into()), port: Some(2200), credential: Some("bastion-key".into()), jump_host: Some("outer".into()), ..Default::default()
        })).unwrap();

        let mut out = Vec::new();
        resolve_jumps(&store, &vault, "bastion", "deflt", &Auth::Password("pw".into()), &mut out, 0).map_err(|e| e.message).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].host.as_str(), out[0].user.as_str()), ("outer.example", "o"));
        assert!(matches!(out[0].auth, Auth::Password(_)), "no credential → target's auth");
        assert_eq!((out[1].host.as_str(), out[1].port, out[1].user.as_str()), ("b.example", 2200, "deflt"));
        assert!(matches!(out[1].auth, Auth::Key { ref key, .. } if key == "/k"));
    }

    #[test]
    fn jump_loops_are_cut() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let vault = FileVault::open(tempfile::tempdir().unwrap().path());
        store.upsert(&node("a", "a", None, Settings { host: Some("a.h".into()), jump_host: Some("b".into()), ..Default::default() })).unwrap();
        store.upsert(&node("b", "b", None, Settings { host: Some("b.h".into()), jump_host: Some("a".into()), ..Default::default() })).unwrap();
        let mut out = Vec::new();
        let err = resolve_jumps(&store, &vault, "a", "u", &Auth::Password("p".into()), &mut out, 0).err().unwrap();
        assert!(err.message.contains("too deep"), "{}", err.message);
    }
}

// ---------------------------------------------------------------- files (SFTP pane / editor)
// `session` = None means the local disk (editor opening a local file); a local PTY
// session also resolves to the local disk, an SSH session to its SFTP subsystem.

// `via` = "shell" routes through the interactive shell (ShellFs) instead of the
// session's own file access — for browsing as root after `sudo -i`.
async fn fs_for(state: &AppState, session: Option<&str>, via: Option<&str>) -> CmdResult<std::sync::Arc<dyn crate::sftp::Fs>> {
    match session.filter(|s| !s.is_empty()) {
        None => Ok(std::sync::Arc::new(crate::sftp::LocalFs)),
        Some(id) => match via {
            // Root SFTP over sudo (P6): opened by sftp_sudo_open, cached in AppState.
            Some("sudo") => state
                .sudo_fs
                .lock()
                .await
                .get(id)
                .cloned()
                .ok_or_else(|| "no sudo SFTP session; open one first".to_string()),
            Some("shell") => Ok(std::sync::Arc::new(crate::sftp::ShellFs { session: state.sessions.get(id).await.map_err(err)? })),
            _ => state.sessions.get(id).await.map_err(err)?.fs().await.map_err(err),
        },
    }
}

/// Standard sftp-server locations, then $PATH.
const SFTP_SERVER_PROBE: &str = "for p in /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server /usr/lib/sftp-server \"$(command -v sftp-server 2>/dev/null)\"; do [ -x \"$p\" ] && { printf %s \"$p\"; break; }; done";

/// Outcome of a sudo-sftp attempt — always Ok so the UI can show why it fell back
/// (the reason also goes to the log). `ok=false` means use the shell backend.
#[derive(serde::Serialize)]
pub struct SudoOutcome {
    pub ok: bool,
    /// The sftp-server path that was probed, or None if not found.
    pub probe: Option<String>,
    /// "ok" | "refused" | "timeout" | "skipped" | "error"
    pub sudo: &'static str,
    pub reason: String,
}

/// Open a root SFTP session over sudo. `password` is used if given, else the
/// session's own SSH password (never returned to the UI). On success later
/// sftp_* calls with via="sudo" go through it.
#[tauri::command]
pub async fn sftp_sudo_open(state: State<'_, AppState>, session: String, password: Option<String>) -> CmdResult<SudoOutcome> {
    let sess = state.sessions.get(&session).await.map_err(err)?;
    let (out, code) = sess.exec_hidden(SFTP_SERVER_PROBE).await.map_err(err)?;
    let server = crate::hidden::strip_escapes(&String::from_utf8_lossy(&out)).trim().to_string();
    if server.is_empty() {
        tracing::warn!(session = %session, probe_exit = code, "sudo-sftp: probe=none");
        return Ok(SudoOutcome { ok: false, probe: None, sudo: "skipped", reason: "sftp-server not found on the host".into() });
    }
    let pw = match password.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => sess.ssh_password().await.ok_or_else(|| "a sudo password is required".to_string())?,
    };
    // sudo can hang forever if the host has `Defaults requiretty` (no tty on an
    // exec channel) or is waiting on a prompt; cap the handshake.
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(12), sess.sudo_fs(&pw, &server)).await {
        Ok(Ok(fs)) => {
            state.sudo_fs.lock().await.insert(session.clone(), fs);
            SudoOutcome { ok: true, probe: Some(server.clone()), sudo: "ok", reason: String::new() }
        }
        Ok(Err(e)) => {
            let reason = format!("{e:#}");
            let low = reason.to_lowercase();
            let sudo = if low.contains("tty") {
                "refused" // requiretty
            } else if low.contains("incorrect")
                || low.contains("try again")
                || low.contains("authentication")
                || low.contains("eof")
                || low.contains("version")
                || low.contains("did not start")
            {
                "refused"
            } else {
                "error"
            };
            SudoOutcome { ok: false, probe: Some(server.clone()), sudo, reason }
        }
        Err(_) => SudoOutcome {
            ok: false,
            probe: Some(server.clone()),
            sudo: "timeout",
            reason: "sudo did not respond within 12s (requiretty, or waiting on a prompt?)".into(),
        },
    };
    tracing::info!(session = %session, probe = %server, sudo = outcome.sudo, reason = %outcome.reason, "sudo-sftp");
    Ok(outcome)
}

/// Whether the session has a stored SSH password sudo could reuse (for the UI's
/// "use saved password" offer). The password itself never crosses this boundary.
#[tauri::command]
pub async fn sftp_sudo_has_password(state: State<'_, AppState>, session: String) -> CmdResult<bool> {
    Ok(state.sessions.get(&session).await.map_err(err)?.ssh_password().await.is_some())
}

#[tauri::command]
pub async fn sftp_sudo_clear(state: State<'_, AppState>, session: String) -> CmdResult<()> {
    state.sudo_fs.lock().await.remove(&session);
    Ok(())
}

#[tauri::command]
pub async fn sftp_home(state: State<'_, AppState>, session: Option<String>, via: Option<String>) -> CmdResult<String> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.home().await.map_err(err)
}

#[tauri::command]
pub async fn sftp_list(state: State<'_, AppState>, session: Option<String>, via: Option<String>, path: String) -> CmdResult<crate::sftp::Listing> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.list(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_stat(state: State<'_, AppState>, session: Option<String>, via: Option<String>, path: String) -> CmdResult<crate::sftp::Entry> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.stat(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_mkdir(state: State<'_, AppState>, session: Option<String>, via: Option<String>, path: String) -> CmdResult<()> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.mkdir(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_rename(state: State<'_, AppState>, session: Option<String>, via: Option<String>, from: String, to: String) -> CmdResult<()> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.rename(&from, &to).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_delete(state: State<'_, AppState>, session: Option<String>, via: Option<String>, path: String, is_dir: bool) -> CmdResult<()> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.delete(&path, is_dir).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_chmod(state: State<'_, AppState>, session: Option<String>, via: Option<String>, path: String, mode: u32) -> CmdResult<()> {
    fs_for(&state, session.as_deref(), via.as_deref()).await?.chmod(&path, mode).await.map_err(err)
}

/// Runs the transfer to completion (progress arrives as `sftp:progress` events
/// keyed by `transfer`); the result is the final verdict.
async fn run_transfer<F, Fut>(state: &AppState, transfer: &str, f: F) -> CmdResult<()>
where
    F: FnOnce(crate::sftp::Transfer) -> Fut,
    Fut: std::future::Future<Output = (crate::sftp::Transfer, anyhow::Result<()>)>,
{
    let flag = state.transfers.start(transfer);
    let app = state.sessions.app().clone();
    let emit = move |p: crate::sftp::Progress| { let _ = tauri::Emitter::emit(&app, "sftp:progress", p); };
    let t = crate::sftp::Transfer::new(transfer.to_string(), Some(Box::new(emit)), flag);
    let (t, r) = f(t).await;
    state.transfers.finish(transfer);
    match r {
        Ok(()) => {
            t.report(0, 0, "done", None);
            Ok(())
        }
        Err(e) => {
            let state_name = if t.cancelled() { "cancelled" } else { "error" };
            t.report(0, 0, state_name, Some(format!("{e:#}")));
            Err(format!("{e:#}"))
        }
    }
}

#[tauri::command]
pub async fn sftp_get(state: State<'_, AppState>, session: Option<String>, via: Option<String>, transfer: String, remote: String, local: String) -> CmdResult<()> {
    let fs = fs_for(&state, session.as_deref(), via.as_deref()).await?;
    run_transfer(&state, &transfer, |t| async move {
        let r = fs.download(&remote, std::path::Path::new(&local), &t).await;
        (t, r)
    })
    .await
}

#[tauri::command]
pub async fn sftp_put(state: State<'_, AppState>, session: Option<String>, via: Option<String>, transfer: String, local: String, remote: String) -> CmdResult<()> {
    let fs = fs_for(&state, session.as_deref(), via.as_deref()).await?;
    run_transfer(&state, &transfer, |t| async move {
        let r = fs.upload(std::path::Path::new(&local), &remote, &t).await;
        (t, r)
    })
    .await
}

#[tauri::command]
pub fn sftp_cancel(state: State<'_, AppState>, transfer: String) -> bool {
    state.transfers.cancel(&transfer)
}

// ---------------------------------------------------------------- editor

/// Editor files are held in memory: refuse anything larger than this.
const EDITOR_LIMIT: u64 = 64 * 1024 * 1024;

#[tauri::command]
pub async fn file_read(state: State<'_, AppState>, session: Option<String>, via: Option<String>, path: String) -> CmdResult<crate::textfile::TextFile> {
    let bytes = fs_for(&state, session.as_deref(), via.as_deref()).await?.read(&path, EDITOR_LIMIT).await.map_err(err)?;
    if crate::textfile::looks_binary(&bytes) {
        return Err(format!("{path} looks like a binary file"));
    }
    Ok(crate::textfile::decode(&bytes))
}

/// Atomic replace through `Fs::write` (temp file + rename).
#[tauri::command]
pub async fn file_write(
    state: State<'_, AppState>,
    session: Option<String>,
    via: Option<String>,
    path: String,
    text: String,
    encoding: crate::textfile::Encoding,
    eol: crate::textfile::Eol,
) -> CmdResult<u64> {
    let bytes = crate::textfile::encode(&text, encoding, eol);
    fs_for(&state, session.as_deref(), via.as_deref()).await?.write(&path, &bytes).await.map_err(err)?;
    Ok(bytes.len() as u64)
}

// ---------------------------------------------------------------- docs / flag help (P5)

/// Cache dir for `<cmd> --help` output, one file per command basename.
fn help_cache_dir(state: &AppState) -> std::path::PathBuf {
    state.paths.data_dir.join("help-cache")
}

/// Only cache/lookup safe command basenames (no path separators / metacharacters).
fn safe_cmd(cmd: &str) -> Option<String> {
    let base = cmd.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(cmd);
    if !base.is_empty() && base.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+')) {
        Some(base.to_string())
    } else {
        None
    }
}

/// `<cmd> --help` over the session, hidden and cached per host in data/help-cache.
/// Returns "" when the command has no --help; the UI then relies on tldr alone.
#[tauri::command]
pub async fn help_fetch(state: State<'_, AppState>, session: String, cmd: String) -> CmdResult<String> {
    let base = safe_cmd(&cmd).ok_or_else(|| "unsafe command name".to_string())?;
    // Cache key includes the host so two boxes don't share a version's flags.
    let key = format!("{}@{base}", session.get(..8).unwrap_or(&session));
    let path = help_cache_dir(&state).join(&key);
    if let Ok(text) = tokio::fs::read_to_string(&path).await {
        return Ok(text);
    }
    let sess = state.sessions.get(&session).await.map_err(err)?;
    // Try --help then -h; strip escapes, keep it bounded.
    let script = format!("{b} --help 2>&1 || {b} -h 2>&1 || true", b = crate::hidden::sq(&base));
    let (out, _) = sess.exec_hidden(&format!("{script} | head -n 300")).await.map_err(err)?;
    let text = crate::hidden::strip_escapes(&String::from_utf8_lossy(&out));
    let _ = tokio::fs::create_dir_all(help_cache_dir(&state)).await;
    let _ = tokio::fs::write(&path, &text).await;
    Ok(text)
}

// ---------------------------------------------------------------- MCP (R2-4)

#[derive(Serialize)]
pub struct McpStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub token: String,
    pub default_tier: String,
}

#[tauri::command]
pub async fn mcp_status(state: State<'_, AppState>) -> CmdResult<McpStatus> {
    let (enabled, default_tier) = {
        let p = state.profile.read().unwrap();
        (p.mcp.enabled, p.mcp.default_tier.clone())
    };
    Ok(McpStatus {
        enabled,
        running: state.mcp.is_running(),
        port: *state.mcp.port.lock().await,
        token: state.mcp.token.lock().await.clone(),
        default_tier,
    })
}

/// Persist `enabled` in the profile and start/stop the server to match.
#[tauri::command]
pub async fn mcp_set_enabled(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> CmdResult<McpStatus> {
    let port = {
        let mut p = state.profile.write().unwrap();
        p.mcp.enabled = enabled;
        crate::profile::save(&state.paths.data_dir, &p).map_err(err)?;
        p.mcp.port
    };
    if enabled {
        crate::mcp::start(&app, &state, port).await.map_err(err)?;
    } else {
        crate::mcp::stop(&state).await;
    }
    mcp_status(state).await
}

/// New per-launch bearer token; restarts the endpoint so it takes effect.
#[tauri::command]
pub async fn mcp_regen_token(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<McpStatus> {
    *state.mcp.token.lock().await = crate::mcp::gen_token();
    if state.mcp.is_running() {
        crate::mcp::stop(&state).await;
        let port = state.profile.read().unwrap().mcp.port;
        crate::mcp::start(&app, &state, port).await.map_err(err)?;
    }
    mcp_status(state).await
}

/// The UI answers an approval / scrollback / open-editor round-trip.
#[tauri::command]
pub async fn mcp_answer(state: State<'_, AppState>, req_id: String, value: serde_json::Value) -> CmdResult<()> {
    state.mcp.answer(&req_id, value).await;
    Ok(())
}

/// Config snippet to paste into a client. `kind`: "claude" (Claude Code / Cursor
/// mcpServers JSON) or "stdio" (command form). The token is embedded.
#[tauri::command]
pub async fn mcp_config(state: State<'_, AppState>, kind: String) -> CmdResult<String> {
    let port = state.mcp.port.lock().await.unwrap_or(0);
    let token = state.mcp.token.lock().await.clone();
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "charon".into());
    let snippet = match kind.as_str() {
        "stdio" => serde_json::json!({
            "mcpServers": { "charon": { "command": exe, "args": ["--mcp-stdio"] } }
        }),
        // Default: HTTP with the bearer token.
        _ => serde_json::json!({
            "mcpServers": { "charon": {
                "type": "http",
                "url": format!("http://127.0.0.1:{port}/mcp"),
                "headers": { "Authorization": format!("Bearer {token}") }
            } }
        }),
    };
    Ok(serde_json::to_string_pretty(&snippet).unwrap_or_default())
}

/// Tail of data/mcp.log for the live call log (most recent last).
#[tauri::command]
pub async fn mcp_log(state: State<'_, AppState>, lines: Option<usize>) -> CmdResult<Vec<serde_json::Value>> {
    let path = state.paths.data_dir.join("mcp.log");
    let text = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let n = lines.unwrap_or(200);
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(n);
    Ok(all[start..].iter().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// `man <cmd>` over the active session, hidden and escape-stripped, for the docs
/// pane. `col -bx` flattens overstrike/backspaces; bounded so a huge page is fine.
#[tauri::command]
pub async fn man_fetch(state: State<'_, AppState>, session: String, cmd: String) -> CmdResult<String> {
    let base = safe_cmd(&cmd).ok_or_else(|| "unsafe command name".to_string())?;
    let sess = state.sessions.get(&session).await.map_err(err)?;
    let script = format!("man -P cat {b} 2>/dev/null | col -bx | head -n 500", b = crate::hidden::sq(&base));
    let (out, code) = sess.exec_hidden(&script).await.map_err(err)?;
    let text = crate::hidden::strip_escapes(&String::from_utf8_lossy(&out));
    if code != 0 || text.trim().is_empty() {
        return Err(format!("no man page for {base} on this host"));
    }
    Ok(text)
}

/// Download the latest tldr pages into data/tldr.json.gz. Returns the page count.
#[tauri::command]
pub async fn tldr_update(state: State<'_, AppState>) -> CmdResult<usize> {
    crate::docs::update(&state.paths.data_dir).await.map_err(err)
}

/// The gzipped tldr bundle (data-dir override, else the embedded build-time copy)
/// as base64. Served over IPC because the webview CSP blocks fetching a static asset.
#[tauri::command]
pub async fn tldr_bundle(state: State<'_, AppState>) -> CmdResult<String> {
    let data_dir = state.paths.data_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || crate::docs::bundle_bytes(&data_dir)).await.map_err(err)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}
