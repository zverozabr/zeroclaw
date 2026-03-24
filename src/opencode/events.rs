//! SSE event consumer for the OpenCode server.
//!
//! Parses the raw `GET /event` byte stream, filters by session ID, and
//! produces typed [`OpenCodeEvent`] values that drive [`StatusBuilder`].

use serde::Deserialize;
use tracing::debug;

use crate::opencode::status::StatusBuilder;

// ── Event type ────────────────────────────────────────────────────────────────

/// Events emitted by the OpenCode server during a session.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenCodeEvent {
    /// Incremental text from the assistant response.
    TextDelta(String),
    /// Incremental text from the model's thinking/reasoning.
    ThinkingDelta(String),
    /// A tool call started.
    ToolStart { name: String },
    /// A tool call completed or errored.
    ToolEnd { name: String },
    /// The session finished generating (status = "idle").
    SessionIdle,
    /// Server heartbeat — resets the inactivity timer.
    Heartbeat,
    /// SSE subscription confirmed by server.
    Connected,
}

// ── Private wire types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SseEnvelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    properties: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartDeltaProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    field: String,
    #[serde(default)]
    delta: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartUpdatedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "type")]
    part_type: Option<String>,
    tool_name: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStatusProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    status: String,
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse one SSE `data:` payload for the given session.
///
/// Returns `None` for unrecognised events, events from other sessions,
/// or parse failures. Never panics.
pub fn parse_sse_event(raw_data: &str, session_id: &str) -> Option<OpenCodeEvent> {
    let envelope: SseEnvelope = match serde_json::from_str(raw_data) {
        Ok(e) => e,
        Err(e) => {
            debug!(error = %e, "failed to parse SSE envelope");
            return None;
        }
    };

    match envelope.kind.as_str() {
        "message.part.delta" => {
            let props: PartDeltaProps = serde_json::from_value(envelope.properties).ok()?;
            if props.session_id != session_id {
                return None;
            }
            match props.field.as_str() {
                "text" => Some(OpenCodeEvent::TextDelta(props.delta)),
                "thinking" | "reasoning" => Some(OpenCodeEvent::ThinkingDelta(props.delta)),
                _ => None,
            }
        }

        "message.part.updated" => {
            let props: PartUpdatedProps = serde_json::from_value(envelope.properties).ok()?;
            if props.session_id != session_id {
                return None;
            }
            if props.part_type.as_deref() != Some("tool-invocation") {
                return None;
            }
            let name = props.tool_name.unwrap_or_else(|| "unknown".to_string());
            match props.state.as_deref() {
                Some("running") => Some(OpenCodeEvent::ToolStart { name }),
                Some("result" | "error") => Some(OpenCodeEvent::ToolEnd { name }),
                _ => None,
            }
        }

        "session.status" => {
            let props: SessionStatusProps = serde_json::from_value(envelope.properties).ok()?;
            if props.session_id != session_id {
                return None;
            }
            if props.status == "idle" {
                Some(OpenCodeEvent::SessionIdle)
            } else {
                None
            }
        }

        "server.heartbeat" => Some(OpenCodeEvent::Heartbeat),
        "server.connected" => Some(OpenCodeEvent::Connected),

        _ => None,
    }
}

// ── StatusBuilder bridge ──────────────────────────────────────────────────────

/// Feed an [`OpenCodeEvent`] into a [`StatusBuilder`].
///
/// Accumulates thinking text in `thinking_buf`. Returns `true` when the
/// session is idle (caller should stop draining events).
///
/// Text deltas are **not** fed to StatusBuilder — the caller accumulates
/// them in its own `text_buf` and calls `on_response_text` at the end.
pub fn drain_sse_into_status(
    event: &OpenCodeEvent,
    status: &mut StatusBuilder,
    thinking_buf: &mut String,
    active_tool: &mut Option<String>,
) -> bool {
    match event {
        OpenCodeEvent::TextDelta(_) | OpenCodeEvent::Heartbeat | OpenCodeEvent::Connected => {
            // TextDelta: accumulates in the caller's text_buf, not here.
            // Heartbeat/Connected: no-op for status; caller resets inactivity timer on Heartbeat.
        }
        OpenCodeEvent::ThinkingDelta(delta) => {
            thinking_buf.push_str(delta);
        }
        OpenCodeEvent::ToolStart { name } => {
            if !thinking_buf.is_empty() {
                status.on_thinking_end(thinking_buf);
                thinking_buf.clear();
            }
            status.on_tool_start(name, &serde_json::Value::Null);
            *active_tool = Some(name.clone());
        }
        OpenCodeEvent::ToolEnd { name } => {
            status.on_tool_end(name, "");
            *active_tool = None;
        }
        OpenCodeEvent::SessionIdle => {
            if !thinking_buf.is_empty() {
                status.on_thinking_end(thinking_buf);
                thinking_buf.clear();
            }
            return true;
        }
    }
    false
}

// ── SSE subscription ──────────────────────────────────────────────────────────

/// Subscribe to the OpenCode event stream for the given session.
///
/// Returns a receiver of parsed events, a cancellation token to stop
/// the reader, and the JoinHandle of the reader task.
///
/// The SSE reader reconnects automatically on disconnect with exponential
/// backoff (500ms → 30s).
pub fn subscribe_sse(
    client: reqwest::Client,
    base_url: String,
    session_id: String,
) -> (
    tokio::sync::mpsc::Receiver<OpenCodeEvent>,
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(128);
    let token = tokio_util::sync::CancellationToken::new();
    let handle = tokio::spawn(sse_reader_task(
        client,
        base_url,
        session_id,
        tx,
        token.clone(),
    ));
    (rx, token, handle)
}

async fn sse_reader_task(
    client: reqwest::Client,
    base_url: String,
    session_id: String,
    tx: tokio::sync::mpsc::Sender<OpenCodeEvent>,
    stop: tokio_util::sync::CancellationToken,
) {
    use futures_util::StreamExt as _;

    let mut backoff = std::time::Duration::from_millis(500);
    const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

    loop {
        if stop.is_cancelled() {
            return;
        }

        let url = format!("{}/event", base_url);
        let connect_result = client.get(&url).send().await;

        let response = match connect_result {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "SSE /event returned non-2xx");
                if stop.is_cancelled() {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "SSE /event connect failed");
                if stop.is_cancelled() {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        // Connected — reset backoff
        backoff = std::time::Duration::from_millis(500);

        let mut stream = response.bytes_stream();
        let mut bytes_buf: Vec<u8> = Vec::with_capacity(4096);

        loop {
            tokio::select! {
                () = stop.cancelled() => {
                    debug!(session_id = %session_id, "SSE reader cancelled");
                    return;
                }
                chunk = stream.next() => {
                    match chunk {
                        None => break, // stream closed → reconnect
                        Some(Err(e)) => {
                            tracing::warn!(error = %e, "SSE stream error, reconnecting");
                            break;
                        }
                        Some(Ok(bytes)) => {
                            bytes_buf.extend_from_slice(&bytes);
                            // Drain complete newline-terminated lines
                            while let Some(pos) = bytes_buf.iter().position(|&b| b == b'\n') {
                                let line_bytes: Vec<u8> = bytes_buf.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&line_bytes);
                                let line = line.trim_end_matches('\n').trim_end_matches('\r');
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if let Some(ev) = parse_sse_event(data, &session_id) {
                                        // Reset backoff on successful event
                                        if matches!(ev, OpenCodeEvent::Connected) {
                                            backoff = std::time::Duration::from_millis(500);
                                        }
                                        if tx.send(ev).await.is_err() {
                                            // Receiver dropped — stop
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Reconnect
        if stop.is_cancelled() {
            return;
        }
        tracing::info!(
            session_id = %session_id,
            backoff_ms = backoff.as_millis(),
            "SSE disconnected, reconnecting"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to build a JSON data line
    fn delta_json(session_id: &str, field: &str, delta: &str) -> String {
        format!(
            r#"{{"type":"message.part.delta","properties":{{"sessionID":"{session_id}","messageID":"m","partID":"p","field":"{field}","delta":"{delta}"}}}}"#
        )
    }

    fn tool_json(session_id: &str, state: &str, name: &str) -> String {
        format!(
            r#"{{"type":"message.part.updated","properties":{{"sessionID":"{session_id}","type":"tool-invocation","toolName":"{name}","state":"{state}"}}}}"#
        )
    }

    fn status_json(session_id: &str, status: &str) -> String {
        format!(
            r#"{{"type":"session.status","properties":{{"sessionID":"{session_id}","status":"{status}"}}}}"#
        )
    }

    #[test]
    fn parse_text_delta_correct_session() {
        let ev = parse_sse_event(&delta_json("ses_abc", "text", "Hello"), "ses_abc");
        assert_eq!(ev, Some(OpenCodeEvent::TextDelta("Hello".into())));
    }

    #[test]
    fn parse_text_delta_wrong_session_filtered() {
        let ev = parse_sse_event(&delta_json("ses_other", "text", "Hello"), "ses_abc");
        assert!(ev.is_none());
    }

    #[test]
    fn parse_thinking_delta() {
        let ev = parse_sse_event(
            &delta_json("ses_abc", "thinking", "Let me think"),
            "ses_abc",
        );
        assert_eq!(
            ev,
            Some(OpenCodeEvent::ThinkingDelta("Let me think".into()))
        );
    }

    #[test]
    fn parse_reasoning_delta_alias() {
        let ev = parse_sse_event(&delta_json("ses_abc", "reasoning", "Hmm"), "ses_abc");
        assert_eq!(ev, Some(OpenCodeEvent::ThinkingDelta("Hmm".into())));
    }

    #[test]
    fn parse_tool_start() {
        let ev = parse_sse_event(&tool_json("ses_abc", "running", "bash"), "ses_abc");
        assert_eq!(
            ev,
            Some(OpenCodeEvent::ToolStart {
                name: "bash".into()
            })
        );
    }

    #[test]
    fn parse_tool_end_result() {
        let ev = parse_sse_event(&tool_json("ses_abc", "result", "bash"), "ses_abc");
        assert_eq!(
            ev,
            Some(OpenCodeEvent::ToolEnd {
                name: "bash".into()
            })
        );
    }

    #[test]
    fn parse_tool_end_error() {
        let ev = parse_sse_event(&tool_json("ses_abc", "error", "grep"), "ses_abc");
        assert_eq!(
            ev,
            Some(OpenCodeEvent::ToolEnd {
                name: "grep".into()
            })
        );
    }

    #[test]
    fn parse_session_idle() {
        let ev = parse_sse_event(&status_json("ses_abc", "idle"), "ses_abc");
        assert_eq!(ev, Some(OpenCodeEvent::SessionIdle));
    }

    #[test]
    fn parse_session_busy_returns_none() {
        let ev = parse_sse_event(&status_json("ses_abc", "busy"), "ses_abc");
        assert!(ev.is_none());
    }

    #[test]
    fn parse_heartbeat_no_session_filter() {
        let raw = r#"{"type":"server.heartbeat","properties":{}}"#;
        assert_eq!(
            parse_sse_event(raw, "any_session"),
            Some(OpenCodeEvent::Heartbeat)
        );
    }

    #[test]
    fn parse_connected() {
        let raw = r#"{"type":"server.connected","properties":{}}"#;
        assert_eq!(parse_sse_event(raw, "any"), Some(OpenCodeEvent::Connected));
    }

    #[test]
    fn parse_unknown_type_returns_none() {
        let raw = r#"{"type":"session.created","properties":{}}"#;
        assert!(parse_sse_event(raw, "ses_abc").is_none());
    }

    #[test]
    fn parse_invalid_json_returns_none() {
        assert!(parse_sse_event("not json", "ses_abc").is_none());
    }

    #[test]
    fn parse_delta_unknown_field_filtered() {
        let ev = parse_sse_event(&delta_json("ses_abc", "code", "fn main()"), "ses_abc");
        assert!(ev.is_none());
    }

    #[test]
    fn drain_accumulates_thinking() {
        let mut status = StatusBuilder::new();
        let mut thinking_buf = String::new();
        let mut active_tool = None;

        let done = drain_sse_into_status(
            &OpenCodeEvent::ThinkingDelta("part1".into()),
            &mut status,
            &mut thinking_buf,
            &mut active_tool,
        );
        let done2 = drain_sse_into_status(
            &OpenCodeEvent::ThinkingDelta("part2".into()),
            &mut status,
            &mut thinking_buf,
            &mut active_tool,
        );
        assert!(!done && !done2);
        assert_eq!(thinking_buf, "part1part2");
    }

    #[test]
    fn drain_flushes_thinking_on_tool_start() {
        let mut status = StatusBuilder::new();
        let mut thinking_buf = "some thought".to_string();
        let mut active_tool = None;

        drain_sse_into_status(
            &OpenCodeEvent::ToolStart {
                name: "bash".into(),
            },
            &mut status,
            &mut thinking_buf,
            &mut active_tool,
        );
        assert!(thinking_buf.is_empty());
        assert!(status.render().contains('\u{1f4ad}')); // 💭
    }

    #[test]
    fn drain_session_idle_returns_true() {
        let mut status = StatusBuilder::new();
        let mut thinking_buf = String::new();
        let mut active_tool = None;
        let done = drain_sse_into_status(
            &OpenCodeEvent::SessionIdle,
            &mut status,
            &mut thinking_buf,
            &mut active_tool,
        );
        assert!(done);
    }

    #[test]
    fn drain_flushes_thinking_on_idle() {
        let mut status = StatusBuilder::new();
        let mut thinking_buf = "thought".to_string();
        let mut active_tool = None;
        let done = drain_sse_into_status(
            &OpenCodeEvent::SessionIdle,
            &mut status,
            &mut thinking_buf,
            &mut active_tool,
        );
        assert!(done);
        assert!(thinking_buf.is_empty());
        assert!(status.render().contains('\u{1f4ad}')); // 💭
    }
}
