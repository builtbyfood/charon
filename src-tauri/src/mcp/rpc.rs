//! Minimal JSON-RPC 2.0 for MCP: parse a request, build a response. MCP methods
//! we implement: `initialize`, `notifications/initialized` (no reply),
//! `tools/list`, `tools/call`, and `ping`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    /// Absent for notifications.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl Response {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    pub fn err(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self { jsonrpc: "2.0", id, result: None, error: Some(RpcError { code, message: message.into() }) }
    }
}

/// A tool result in MCP shape: `{ content: [{type:"text", text}], isError? }`.
pub fn tool_text(value: &Value) -> Value {
    let text = if value.is_string() {
        value.as_str().unwrap().to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    };
    json!({ "content": [{ "type": "text", "text": text }] })
}

pub fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

/// Server identity + capabilities for `initialize`.
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "charon", "version": env!("CARGO_PKG_VERSION") }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_text_stringifies_json_but_passes_strings() {
        assert_eq!(tool_text(&json!("hi"))["content"][0]["text"], json!("hi"));
        let v = tool_text(&json!({ "a": 1 }));
        assert!(v["content"][0]["text"].as_str().unwrap().contains("\"a\""));
    }

    #[test]
    fn response_omits_null_fields() {
        let r = Response::ok(Some(json!(1)), json!({}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("error"));
        assert!(s.contains("\"id\":1"));
    }
}
