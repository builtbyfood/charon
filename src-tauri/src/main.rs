// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `--mcp-stdio`: run as a thin MCP stdio proxy to the running app, not the GUI.
    if std::env::args().any(|a| a == "--mcp-stdio") {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = rt.block_on(charon_lib::mcp::stdio::run()) {
            eprintln!("charon --mcp-stdio: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    charon_lib::run()
}
