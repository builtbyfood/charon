//! `charon --mcp-stdio`: a thin stdio↔HTTP proxy. An MCP client (that speaks
//! stdio) spawns this; it forwards each newline-delimited JSON-RPC message to the
//! running app's local HTTP endpoint and writes the reply back. The app stays the
//! single owner of sessions; this process holds no state.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn run() -> anyhow::Result<()> {
    let paths = crate::paths::AppPaths::detect()?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let id = serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|v| v.get("id").cloned());
        let reply = match crate::mcp::read_endpoint(&paths.data_dir) {
            None => Some(rpc_error(id, "Charon is not running (start the app to use its MCP server)")),
            Some((port, token)) => match crate::mcp::http::post(port, &token, &line).await {
                Ok(Some(resp)) => Some(resp),
                Ok(None) => None, // notification: no reply
                Err(e) => Some(rpc_error(id, &format!("cannot reach Charon: {e}"))),
            },
        };
        if let Some(resp) = reply {
            stdout.write_all(resp.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

fn rpc_error(id: Option<serde_json::Value>, message: &str) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": message } }).to_string()
}
