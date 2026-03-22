use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::time::timeout;

/// Events emitted by the Pi coding agent during a prompt session.
#[derive(Debug, Clone, PartialEq)]
pub enum PiEvent {
    ThinkingDelta(String),
    ThinkingEnd(String),
    ToolStart {
        name: String,
        args: serde_json::Value,
    },
    ToolEnd {
        name: String,
        output: String,
    },
    TextDelta(String),
    AgentEnd {
        text: String,
    },
}

/// Parse a single JSONL event from Pi into a `PiEvent`.
///
/// Returns `None` for unrecognised or irrelevant event types.
pub fn parse_pi_event(event: &serde_json::Value) -> Option<PiEvent> {
    let event_type = event.get("type")?.as_str()?;

    match event_type {
        "message_update" => {
            let ame = event.get("assistantMessageEvent")?;
            let sub_type = ame.get("type")?.as_str()?;
            match sub_type {
                "thinking_delta" => {
                    let delta = ame.get("delta")?.as_str()?.to_owned();
                    Some(PiEvent::ThinkingDelta(delta))
                }
                "thinking_end" => {
                    let content = ame
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    Some(PiEvent::ThinkingEnd(content))
                }
                "text_delta" => {
                    let delta = ame.get("delta")?.as_str()?.to_owned();
                    Some(PiEvent::TextDelta(delta))
                }
                _ => None,
            }
        }
        "tool_execution_start" => {
            let name = event.get("toolName")?.as_str()?.to_owned();
            let args = event
                .get("args")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(PiEvent::ToolStart { name, args })
        }
        "tool_execution_end" => {
            let name = event.get("toolName")?.as_str()?.to_owned();
            let output = event
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            Some(PiEvent::ToolEnd { name, output })
        }
        "agent_end" => {
            let text = extract_last_assistant_text(event);
            Some(PiEvent::AgentEnd { text })
        }
        _ => None,
    }
}

/// Extract the text of the last assistant message from the `messages` array in
/// an `agent_end` event.
fn extract_last_assistant_text(event: &serde_json::Value) -> String {
    let messages = match event.get("messages").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return String::new(),
    };

    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
            // Try "text" field first, then "content"
            if let Some(text) = msg.get("text").and_then(|v| v.as_str()) {
                return text.to_owned();
            }
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                return content.to_owned();
            }
            // content might be an array of blocks
            if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                for block in blocks {
                    if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            return t.to_owned();
                        }
                    }
                }
            }
        }
    }

    String::new()
}

// ---------------------------------------------------------------------------
// Async I/O helpers
// ---------------------------------------------------------------------------

/// Send a JSON value as a single JSONL line to the child's stdin.
pub async fn send(stdin: &mut ChildStdin, msg: &serde_json::Value) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

/// Read a single JSONL line from the child's stdout with a timeout.
///
/// Returns `None` on timeout, EOF, or parse failure.
pub async fn recv_line(
    reader: &mut BufReader<ChildStdout>,
    dur: Duration,
) -> Option<serde_json::Value> {
    let mut buf = String::new();
    let result = timeout(dur, reader.read_line(&mut buf)).await;
    match result {
        Ok(Ok(n)) if n > 0 => serde_json::from_str(buf.trim()).ok(),
        _ => None,
    }
}

/// Read lines until we get a response whose `"type"` matches `command`, or
/// until timeout elapses.
pub async fn recv_response(
    reader: &mut BufReader<ChildStdout>,
    command: &str,
    dur: Duration,
) -> Option<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let val = recv_line(reader, remaining).await?;
        if val.get("type").and_then(|v| v.as_str()) == Some(command) {
            return Some(val);
        }
    }
}

// ---------------------------------------------------------------------------
// rpc_prompt — streaming prompt with event callback
// ---------------------------------------------------------------------------

/// Send a prompt to Pi, stream events via `on_event`, and return the final
/// assistant text when `agent_end` is received.
///
/// Automatically cancels `extension_ui_request` events.
pub async fn rpc_prompt<F>(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    message: &str,
    dur: Duration,
    on_event: F,
) -> anyhow::Result<String>
where
    F: Fn(&PiEvent),
{
    // 1. Send prompt
    let prompt = serde_json::json!({
        "type": "prompt",
        "message": message,
    });
    send(stdin, &prompt).await?;

    // 2. Wait for ACK
    let deadline = tokio::time::Instant::now() + dur;
    let ack = recv_response(reader, "prompt", dur)
        .await
        .ok_or_else(|| anyhow::anyhow!("timeout waiting for prompt ACK"))?;

    if ack.get("success").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!("prompt rejected: {}", ack);
    }

    // 3. Stream events
    let mut thinking_buf = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timeout waiting for agent_end");
        }

        let val = match recv_line(reader, remaining).await {
            Some(v) => v,
            None => anyhow::bail!("stream ended without agent_end"),
        };

        // Auto-cancel extension_ui_request
        if val.get("type").and_then(|v| v.as_str()) == Some("extension_ui_request") {
            let cancel = serde_json::json!({
                "type": "extension_ui_response",
                "action": "cancel",
            });
            send(stdin, &cancel).await?;
            continue;
        }

        if let Some(ev) = parse_pi_event(&val) {
            match &ev {
                PiEvent::ThinkingDelta(delta) => {
                    thinking_buf.push_str(delta);
                    on_event(&ev);
                }
                PiEvent::ThinkingEnd(_) => {
                    let end_ev = PiEvent::ThinkingEnd(thinking_buf.clone());
                    on_event(&end_ev);
                    thinking_buf.clear();
                }
                PiEvent::AgentEnd { text } => {
                    let final_text = text.clone();
                    on_event(&ev);
                    return Ok(final_text);
                }
                _ => {
                    on_event(&ev);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Session management RPCs
// ---------------------------------------------------------------------------

/// Create a new Pi session.  Returns `true` on success.
pub async fn rpc_new_session(stdin: &mut ChildStdin, reader: &mut BufReader<ChildStdout>) -> bool {
    let msg = serde_json::json!({ "type": "new_session" });
    if send(stdin, &msg).await.is_err() {
        return false;
    }
    recv_response(reader, "new_session", Duration::from_secs(10))
        .await
        .and_then(|v| v.get("success")?.as_bool())
        .unwrap_or(false)
}

/// Switch to an existing session file.  Returns `true` on success.
pub async fn rpc_switch_session(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    session_file: &str,
) -> bool {
    let msg = serde_json::json!({
        "type": "switch_session",
        "sessionFile": session_file,
    });
    if send(stdin, &msg).await.is_err() {
        return false;
    }
    recv_response(reader, "switch_session", Duration::from_secs(10))
        .await
        .and_then(|v| v.get("success")?.as_bool())
        .unwrap_or(false)
}

/// Retrieve the current session file path.
pub async fn rpc_get_session_file(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
) -> Option<String> {
    let msg = serde_json::json!({ "type": "get_session_file" });
    send(stdin, &msg).await.ok()?;
    let resp = recv_response(reader, "get_state", Duration::from_secs(10)).await?;
    resp.get("sessionFile")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_agent_end_extracts_text() {
        let event = json!({
            "type": "agent_end",
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "text": "Here is the answer." }
            ]
        });
        assert_eq!(
            parse_pi_event(&event),
            Some(PiEvent::AgentEnd {
                text: "Here is the answer.".to_owned()
            })
        );
    }

    #[test]
    fn parse_tool_execution_start() {
        let event = json!({
            "type": "tool_execution_start",
            "toolName": "Read",
            "args": { "file_path": "/tmp/foo.rs" }
        });
        assert_eq!(
            parse_pi_event(&event),
            Some(PiEvent::ToolStart {
                name: "Read".to_owned(),
                args: json!({ "file_path": "/tmp/foo.rs" }),
            })
        );
    }

    #[test]
    fn parse_thinking_delta() {
        let event = json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "thinking_delta",
                "delta": "Let me think..."
            }
        });
        assert_eq!(
            parse_pi_event(&event),
            Some(PiEvent::ThinkingDelta("Let me think...".to_owned()))
        );
    }

    #[test]
    fn parse_text_delta() {
        let event = json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "text_delta",
                "delta": "Hello world"
            }
        });
        assert_eq!(
            parse_pi_event(&event),
            Some(PiEvent::TextDelta("Hello world".to_owned()))
        );
    }

    #[test]
    fn parse_unknown_returns_none() {
        let event = json!({ "type": "some_random_event", "data": 42 });
        assert_eq!(parse_pi_event(&event), None);
    }
}
