//! A Model Context Protocol server over the engine's commands.
//!
//! JSON-RPC 2.0, one message per line (the MCP stdio transport). Agents get the same commands as
//! the menus: `list_commands` describes them and `execute` runs any of them with JSON parameters.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_engine::{COMMANDS, Session};
use serde_json::{Value, json};
use std::io::{BufRead, Read as _, Write};

/// The protocol revision offered when the client does not name one.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The longest message accepted, in bytes. Longer input ends the session instead of exhausting
/// memory.
pub const MAX_MESSAGE_BYTES: u64 = 16 * 1024 * 1024;

/// The transport failed.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Reading or writing the stream failed.
    #[error("MCP transport: {0}")]
    Io(#[from] std::io::Error),
    /// A message was longer than [`MAX_MESSAGE_BYTES`].
    #[error("MCP message longer than {MAX_MESSAGE_BYTES} bytes")]
    MessageTooLong,
}

/// Serves MCP on `input`/`output` until `input` ends.
pub fn serve(session: &mut Session, mut input: impl BufRead, mut output: impl Write) -> Result<(), McpError> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut input).take(MAX_MESSAGE_BYTES + 1).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        if n as u64 > MAX_MESSAGE_BYTES {
            return Err(McpError::MessageTooLong);
        }
        let text = String::from_utf8_lossy(&line);
        if text.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(session, &text) {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
}

/// Handles one JSON-RPC message; returns the reply, or `None` for a notification.
pub fn handle_line(session: &mut Session, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let method = msg.get("method").and_then(Value::as_str);
    let Some(id) = msg.get("id").cloned() else {
        return None; // notifications (initialized, cancelled, …) need no reply
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        Some("initialize") => Ok(initialize(&params)),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => Ok(json!({ "tools": tools() })),
        Some("tools/call") => call_tool(session, &params),
        Some(other) => Err((-32601, format!("method not found: {other}"))),
        None => Err((-32600, "invalid request: no method".to_owned())),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => error(id, code, &message),
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "circuitcraft", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "CircuitCraft: schematic capture and PCB layout. Call list_commands to see every command, then execute to run one with JSON parameters.",
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "list_commands",
            "description": "Every CircuitCraft command: id, label, menu path, shortcut and a description of its JSON parameters.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "execute",
            "description": "Run any CircuitCraft command by id with JSON parameters, exactly as the menus do. Returns the command's JSON result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "enum": COMMANDS.iter().map(|c| c.id).collect::<Vec<_>>() },
                    "params": { "type": "object" },
                },
                "required": ["command"],
            },
        },
    ])
}

fn call_tool(session: &mut Session, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).ok_or((-32602, "tools/call needs a `name`".to_owned()))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let outcome = match name {
        "list_commands" => session.execute("commands", &Value::Null),
        "execute" => {
            let command = args.get("command").and_then(Value::as_str).ok_or((-32602, "execute needs a `command`".to_owned()))?;
            session.execute(command, args.get("params").unwrap_or(&Value::Null))
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    // Command failures are tool results with isError, so the agent can read and react to them.
    Ok(match outcome {
        Ok(v) => {
            json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": wrap(v), "isError": false })
        }
        Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
    })
}

/// `structuredContent` must be an object.
fn wrap(v: Value) -> Value {
    if v.is_object() { v } else { json!({ "result": v }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &str) -> Vec<Value> {
        let mut s = Session::new();
        let mut out = Vec::new();
        serve(&mut s, input.as_bytes(), &mut out).expect("serve");
        String::from_utf8(out).expect("utf8").lines().map(|l| serde_json::from_str(l).expect("json")).collect()
    }

    #[test]
    fn handshake_list_and_call() {
        let replies = run(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"execute","arguments":{"command":"board.new","params":{"name":"Blinky"}}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"execute","arguments":{"command":"board.info"}}}"#,
            "\n",
        ));
        assert_eq!(replies.len(), 4, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "circuitcraft");
        assert_eq!(replies[1]["result"]["tools"][1]["name"], "execute");
        assert_eq!(replies[2]["result"]["isError"], false);
        assert_eq!(replies[3]["result"]["structuredContent"]["name"], "Blinky");
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let replies = run(concat!(
            "not json\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"command":"nope"}}}"#,
            "\n",
        ));
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[1]["error"]["code"], -32601);
        assert_eq!(replies[2]["result"]["isError"], true);
    }

    #[test]
    fn overlong_message_ends_the_session() {
        let mut s = Session::new();
        let huge = vec![b'x'; (MAX_MESSAGE_BYTES + 10) as usize];
        assert!(matches!(serve(&mut s, &huge[..], Vec::new()), Err(McpError::MessageTooLong)));
    }
}
