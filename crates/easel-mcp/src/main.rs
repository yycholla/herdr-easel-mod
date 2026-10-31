//! An MCP server (stdio, JSON-RPC 2.0) with tools that render GPUI code in
//! the GPUI Artifacts viewer, starting the viewer when needed.

mod tools;

use serde_json::{Value, json};
use std::io::{BufRead, Write};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

fn main() -> anyhow::Result<()> {
    let mut state = tools::State::default();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&mut state, &message),
            Err(err) => Some(error(Value::Null, -32700, &format!("parse error: {err}"))),
        };
        if let Some(response) = response {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Handles one message. Notifications (no `id`) get no response.
fn handle(state: &mut tools::State, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let requested = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == requested).unwrap_or(&PROTOCOL_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "gpui-artifacts", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Use render_gpui to show GPUI (Rust) UI code to the user as a live artifact in a Herdr Easel pane or a standalone native viewer."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools::list() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            match tools::call(state, name, &params["arguments"]) {
                Ok(result) => json!({ "content": result.content, "isError": result.is_error }),
                Err(err) => json!({ "content": [{ "type": "text", "text": format!("{err:#}") }], "isError": true }),
            }
        }
        method => return Some(error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_and_lists_tools() {
        let state = &mut tools::State::default();
        let init = handle(state, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle(state, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = handle(state, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let names: Vec<_> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["render_gpui", "get_gpui_artifact", "list_gpui_artifacts", "await_user_message", "send_chat_message", "export_gpui_png"]
        );
        let unknown = handle(state, &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"})).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }
}
