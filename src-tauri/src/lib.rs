//! Charon backend. Tauri commands live in `commands.rs`; everything else is plain Rust
//! so it can be unit-tested and later reused by the MCP server without the UI.

pub mod commands;
pub mod connections;
pub mod docs;
pub mod hidden;
pub mod hostkeys;
pub mod mcp;
pub mod import;
pub mod local;
pub mod serial;
pub mod paths;
pub mod ppk;
pub mod profile;
pub mod session;
pub mod sftp;
pub mod ssh;
pub mod textfile;
pub mod vault;

use std::sync::Arc;
use tauri::Manager;

pub struct AppState {
    pub paths: paths::AppPaths,
    pub sessions: session::SessionManager,
    pub store: Arc<tokio::sync::Mutex<connections::Store>>,
    pub vault: Arc<dyn vault::Vault>,
    pub hostkeys: Arc<hostkeys::HostKeys>,
    pub profile: Arc<std::sync::RwLock<profile::Profile>>,
    pub transfers: sftp::Transfers,
    /// Root SFTP sessions opened via sudo (P6), keyed by session id.
    pub sudo_fs: tokio::sync::Mutex<std::collections::HashMap<String, Arc<dyn sftp::Fs>>>,
    pub mcp: mcp::McpShared,
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "charon=info".into()),
        )
        .init();

    // Data dir first: the WebView2 profile location must be decided before any
    // window exists (see `paths::webview_data_dir`).
    let paths = match paths::AppPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("charon: {e:#}");
            std::process::exit(1);
        }
    };
    tracing::info!(portable = paths.portable, dir = %paths.data_dir.display(), "data dir");
    if let Some(dir) = paths.webview_data_dir() {
        // WebView2 honours this over Tauri's default (%LOCALAPPDATA%\<id>\EBWebView).
        // Two Charons at different integrity levels (an elevated `tauri dev` and a
        // double-clicked exe) sharing one profile leaves the second one deaf to the
        // keyboard: it attaches to the elevated browser process and UIPI blocks
        // focus between the two. Portable and dev builds therefore get their own.
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &dir);
        tracing::info!(dir = %dir.display(), "webview profile");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // WebView2 quirk: the window is shown (and takes Win32 focus) before the
        // webview controller exists, so a DOM `autofocus` shows a caret that gets no
        // keystrokes until the user clicks in the page. Hand the controller keyboard
        // focus once the page is up. Harmless elsewhere.
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                if let Err(e) = webview.set_focus() {
                    tracing::warn!("webview focus: {e}");
                }
            }
        })
        .setup(move |app| {
            let store = connections::Store::open(&paths.data_dir.join("charon.db"))?;
            let vault: Arc<dyn vault::Vault> = Arc::from(vault::open(&paths.data_dir, paths.portable)?);
            let hostkeys = Arc::new(hostkeys::HostKeys::new(&paths.data_dir));
            let profile = Arc::new(std::sync::RwLock::new(profile::load_default(&paths.data_dir)));
            app.manage(AppState {
                hostkeys,
                profile,
                paths,
                sessions: session::SessionManager::new(app.handle().clone()),
                store: Arc::new(tokio::sync::Mutex::new(store)),
                vault,
                transfers: sftp::Transfers::default(),
                sudo_fs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
                mcp: Arc::new(mcp::McpState::default()),
            });
            // Start the MCP server if the profile has it enabled.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    let (enabled, port) = { let p = state.profile.read().unwrap(); (p.mcp.enabled, p.mcp.port) };
                    if enabled {
                        if let Err(e) = mcp::start(&handle, &state, port).await {
                            tracing::warn!("mcp autostart failed: {e:#}");
                        }
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_profile,
            commands::save_profile,
            commands::list_connections,
            commands::save_connection,
            commands::delete_connection,
            commands::vault_status,
            commands::vault_unlock,
            commands::vault_list,
            commands::vault_set,
            commands::vault_delete,
            commands::import_connections,
            commands::import_preview,
            commands::session_open_ssh,
            commands::session_open_local,
            commands::session_open_serial,
            commands::serial_list_ports,
            commands::sftp_home,
            commands::sftp_list,
            commands::sftp_stat,
            commands::sftp_mkdir,
            commands::sftp_rename,
            commands::sftp_delete,
            commands::sftp_chmod,
            commands::sftp_get,
            commands::sftp_put,
            commands::sftp_cancel,
            commands::sftp_sudo_open,
            commands::sftp_sudo_has_password,
            commands::sftp_sudo_clear,
            commands::help_fetch,
            commands::man_fetch,
            commands::tldr_update,
            commands::tldr_bundle,
            commands::mcp_status,
            commands::mcp_set_enabled,
            commands::mcp_regen_token,
            commands::mcp_answer,
            commands::mcp_config,
            commands::mcp_log,
            commands::file_read,
            commands::file_write,
            commands::ssh_hostkey_answer,
            commands::hostkey_forget,
            commands::session_write,
            commands::session_set_shell,
            commands::session_set_cwd,
            commands::session_resize,
            commands::session_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Charon");
}
