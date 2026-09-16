//! MCP over HTTP on 127.0.0.1, hand-rolled on tokio (no extra web dep). One POST
//! per JSON-RPC message; the bearer token guards every request. This is what an
//! HTTP MCP client (Claude Code with a `"type":"http"` server) connects to; the
//! stdio shim proxies to the same endpoint.

use super::{rpc, tools, McpShared};
use crate::AppState;
use serde_json::json;
use std::sync::Arc;
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Handle one JSON-RPC message end to end (shared by both transports).
pub async fn handle_message(app: &tauri::AppHandle, mcp: &McpShared, body: &str) -> Option<String> {
    let req: rpc::Request = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return Some(serde_json::to_string(&rpc::Response::err(None, -32700, format!("parse error: {e}"))).unwrap()),
    };
    // Notifications (no id) get no response.
    let is_notification = req.id.is_none();
    let state = app.state::<AppState>();
    let resp = match req.method.as_str() {
        "initialize" => rpc::Response::ok(req.id, rpc::initialize_result()),
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => rpc::Response::ok(req.id, json!({})),
        "tools/list" => rpc::Response::ok(req.id, json!({ "tools": tools::tool_specs() })),
        "tools/call" => {
            let name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let args = req.params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let result = tools::call(&state, mcp, &name, args).await;
            rpc::Response::ok(req.id, result)
        }
        other => rpc::Response::err(req.id, -32601, format!("method not found: {other}")),
    };
    if is_notification {
        None
    } else {
        Some(serde_json::to_string(&resp).unwrap())
    }
}

/// Bind and serve until `stop` fires. Returns the bound port.
pub async fn serve(app: tauri::AppHandle, mcp: McpShared, port: u16, mut stop: tokio::sync::oneshot::Receiver<()>) -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let bound = listener.local_addr()?.port();
    *mcp.port.lock().await = Some(bound);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop => break,
                accepted = listener.accept() => {
                    if let Ok((sock, _)) = accepted {
                        let (app, mcp) = (app.clone(), mcp.clone());
                        tokio::spawn(async move { let _ = serve_conn(sock, app, mcp).await; });
                    }
                }
            }
        }
    });
    Ok(bound)
}

/// One keep-alive connection: read requests, check the token, reply.
async fn serve_conn(mut sock: TcpStream, app: tauri::AppHandle, mcp: McpShared) -> anyhow::Result<()> {
    let token = mcp.token.lock().await.clone();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        // Read until we have headers + the full body (Content-Length).
        let (head_end, headers, body_start) = loop {
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
                break (pos, headers, pos + 4);
            }
            let n = sock.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let _ = head_end;
        let len: usize = header(&headers, "content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        while buf.len() < body_start + len {
            let n = sock.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body = String::from_utf8_lossy(&buf[body_start..body_start + len]).to_string();
        let method_line = headers.lines().next().unwrap_or("");
        let authed = header(&headers, "authorization").map(|a| a.trim() == format!("Bearer {token}")).unwrap_or(false);

        let reply = if method_line.starts_with("OPTIONS") {
            http_response(204, "", "")
        } else if !method_line.starts_with("POST") {
            http_response(405, "text/plain", "POST only")
        } else if !authed {
            http_response(401, "text/plain", "bad or missing bearer token")
        } else {
            match handle_message(&app, &mcp, &body).await {
                Some(json) => http_response(200, "application/json", &json),
                None => http_response(202, "", ""), // notification: accepted, no body
            }
        };
        sock.write_all(reply.as_bytes()).await?;

        // Consume this request's bytes; keep any pipelined remainder.
        buf.drain(..body_start + len);
        if header(&headers, "connection").map(|c| c.trim().eq_ignore_ascii_case("close")).unwrap_or(false) {
            break;
        }
    }
    Ok(())
}

fn http_response(code: u16, content_type: &str, body: &str) -> String {
    let reason = match code {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        401 => "Unauthorized",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let mut r = format!("HTTP/1.1 {code} {reason}\r\n");
    if !content_type.is_empty() {
        r.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    r.push_str(&format!("Content-Length: {}\r\n", body.len()));
    r.push_str("Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: authorization, content-type\r\n");
    r.push_str("\r\n");
    r.push_str(body);
    r
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v)
        } else {
            None
        }
    })
}

/// Client side (used by the stdio shim): POST one message, return the response body.
pub async fn post(port: u16, token: &str, body: &str) -> anyhow::Result<Option<String>> {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await?;
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(req.as_bytes()).await?;
    let mut resp = Vec::new();
    sock.read_to_end(&mut resp).await?;
    let text = String::from_utf8_lossy(&resp);
    match text.split_once("\r\n\r\n") {
        Some((_, b)) if !b.is_empty() => Ok(Some(b.to_string())),
        _ => Ok(None),
    }
}

pub type SharedApp = Arc<tauri::AppHandle>;
