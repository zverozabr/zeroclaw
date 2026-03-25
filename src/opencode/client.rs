//! HTTP client for the OpenCode server.
//!
//! All ZeroClaw ↔ OpenCode communication goes through this module.
//! The client is stateless — session state lives in `session.rs`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from the OpenCode HTTP client.
#[derive(Debug, thiserror::Error)]
pub enum OpenCodeError {
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("OpenCode server error: HTTP {status}: {body}")]
    ServerError { status: u16, body: String },

    #[error("OpenCode session not found: {session_id}")]
    SessionNotFound { session_id: String },

    #[error("SSE stream timed out after {secs}s of inactivity")]
    SseTimeout { secs: u64 },
}

pub type ClientResult<T> = std::result::Result<T, OpenCodeError>;

// ── Wire types ────────────────────────────────────────────────────────────────

/// Returned by `POST /session`.
#[derive(Debug, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Returned by `POST /session/{id}/message`.
#[derive(Debug, Deserialize)]
pub struct MessageResponse {
    pub info: MessageInfo,
    pub parts: Vec<MessagePart>,
}

#[derive(Debug, Deserialize)]
pub struct MessageInfo {
    pub id: String,
    pub role: String,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MessagePart {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl MessageResponse {
    /// Extract the final assistant text from the response parts.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter(|p| p.kind == "text")
            .filter_map(|p| p.text.as_deref())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Request body for `POST /session/{id}/message`.
#[derive(Serialize)]
struct MessageRequest<'a> {
    parts: Vec<MessagePartRequest<'a>>,
    model: ModelRef<'a>,
    #[serde(rename = "noReply", skip_serializing_if = "Option::is_none")]
    no_reply: Option<bool>,
}

#[derive(Serialize)]
struct MessagePartRequest<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct ModelRef<'a> {
    #[serde(rename = "providerID")]
    provider_id: &'a str,
    #[serde(rename = "modelID")]
    model_id: &'a str,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// HTTP client for the OpenCode server.
pub struct OpenCodeClient {
    http: reqwest::Client,
    base_url: String,
    password: Option<String>,
}

impl OpenCodeClient {
    /// Create a client for the OpenCode server on `127.0.0.1:{port}`.
    ///
    /// Reads `OPENCODE_SERVER_PASSWORD` from the environment for optional
    /// HTTP Basic auth.
    pub fn new(port: u16) -> Self {
        let password = std::env::var("OPENCODE_SERVER_PASSWORD").ok();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("failed to build OpenCode HTTP client");
        Self {
            http,
            base_url: format!("http://127.0.0.1:{port}"),
            password,
        }
    }

    /// Create a client with an explicit base URL — intended for tests.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            password: None,
        }
    }

    // ── Auth helper ───────────────────────────────────────────────────────────

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.password {
            Some(pw) => req.basic_auth("opencode", Some(pw)),
            None => req,
        }
    }

    // ── Health check ──────────────────────────────────────────────────────────

    /// Verify the server is reachable. Returns `Ok(())` on HTTP 2xx.
    pub async fn health_check(&self) -> ClientResult<()> {
        let url = format!("{}/path", self.base_url);
        let resp = self.apply_auth(self.http.get(&url)).send().await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(OpenCodeError::ServerError { status, body })
        }
    }

    // ── Session management ────────────────────────────────────────────────────

    /// Create a new OpenCode session scoped to `directory`.
    ///
    /// Returns the new session ID.
    pub async fn create_session(&self, directory: &str) -> ClientResult<String> {
        let url = format!("{}/session", self.base_url);
        let resp = self
            .apply_auth(self.http.post(&url))
            .header("x-opencode-directory", directory)
            .json(&serde_json::json!({}))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OpenCodeError::ServerError { status, body });
        }
        let info: SessionInfo = resp.json().await?;
        debug!(session_id = %info.id, directory, "created opencode session");
        Ok(info.id)
    }

    /// Retrieve session info. Returns `None` if the session was not found (404).
    pub async fn get_session(&self, session_id: &str) -> ClientResult<Option<SessionInfo>> {
        let url = format!("{}/session/{}", self.base_url, session_id);
        let resp = self.apply_auth(self.http.get(&url)).send().await?;
        match resp.status().as_u16() {
            200..=299 => Ok(Some(resp.json::<SessionInfo>().await?)),
            404 => Ok(None),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(OpenCodeError::ServerError { status, body })
            }
        }
    }

    // ── Messaging ─────────────────────────────────────────────────────────────

    /// Send a message and block until the response is complete.
    ///
    /// Returns the complete `MessageResponse` (contains all parts including
    /// the final assistant text via `.text()`).
    pub async fn send_message(
        &self,
        session_id: &str,
        text: &str,
        provider_id: &str,
        model_id: &str,
    ) -> ClientResult<MessageResponse> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);
        let body = MessageRequest {
            parts: vec![MessagePartRequest { kind: "text", text }],
            model: ModelRef {
                provider_id,
                model_id,
            },
            no_reply: None,
        };
        let resp = self
            .apply_auth(self.http.post(&url))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(OpenCodeError::ServerError {
                status,
                body: body_text,
            });
        }
        Ok(resp.json::<MessageResponse>().await?)
    }

    /// Fetch all messages for a session. Used for polling progress during
    /// prompt execution — new parts (tool calls, thinking) appear incrementally.
    pub async fn get_messages(&self, session_id: &str) -> ClientResult<Vec<MessageResponse>> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);
        let resp = self.apply_auth(self.http.get(&url)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OpenCodeError::ServerError { status, body });
        }
        Ok(resp.json().await?)
    }

    /// Send a message with `noReply: true` — used for history injection.
    ///
    /// OpenCode processes the message but does not generate an AI response.
    pub async fn send_message_no_reply(
        &self,
        session_id: &str,
        text: &str,
        provider_id: &str,
        model_id: &str,
    ) -> ClientResult<()> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);
        let body = MessageRequest {
            parts: vec![MessagePartRequest { kind: "text", text }],
            model: ModelRef {
                provider_id,
                model_id,
            },
            no_reply: Some(true),
        };
        let resp = self
            .apply_auth(self.http.post(&url))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(OpenCodeError::ServerError {
                status,
                body: body_text,
            });
        }
        Ok(())
    }

    /// Send a message asynchronously (fire-and-forget) via `prompt_async`.
    ///
    /// Returns immediately with 204. OpenCode queues the message internally.
    /// Used for `/pf` (follow-up) when the session is busy.
    pub async fn send_message_async(
        &self,
        session_id: &str,
        text: &str,
        provider_id: &str,
        model_id: &str,
    ) -> ClientResult<()> {
        let url = format!("{}/session/{}/prompt_async", self.base_url, session_id);
        let body = MessageRequest {
            parts: vec![MessagePartRequest { kind: "text", text }],
            model: ModelRef {
                provider_id,
                model_id,
            },
            no_reply: None,
        };
        let resp = self
            .apply_auth(self.http.post(&url))
            .json(&body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status == 204 || resp.status().is_success() {
            return Ok(());
        }
        warn!(status, "prompt_async returned non-204");
        let body_text = resp.text().await.unwrap_or_default();
        Err(OpenCodeError::ServerError {
            status,
            body: body_text,
        })
    }

    // ── Control ───────────────────────────────────────────────────────────────

    /// Abort the current in-progress generation for this session.
    ///
    /// Returns `true` if a generation was aborted, `false` if the session
    /// was already idle (not an error).
    pub async fn abort(&self, session_id: &str) -> ClientResult<bool> {
        let url = format!("{}/session/{}/abort", self.base_url, session_id);
        let resp = self.apply_auth(self.http.post(&url)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OpenCodeError::ServerError { status, body });
        }
        Ok(resp.json::<bool>().await?)
    }

    /// Delete an OpenCode session and all its messages.
    ///
    /// Returns `Ok(())` even if the session was not found (idempotent).
    pub async fn delete_session(&self, session_id: &str) -> ClientResult<()> {
        let url = format!("{}/session/{}", self.base_url, session_id);
        let resp = self.apply_auth(self.http.delete(&url)).send().await?;
        match resp.status().as_u16() {
            200..=299 | 404 => Ok(()),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(OpenCodeError::ServerError { status, body })
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> OpenCodeClient {
        OpenCodeClient::with_base_url(server.uri())
    }

    #[tokio::test]
    async fn health_check_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/path"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        make_client(&server).health_check().await.unwrap();
    }

    #[tokio::test]
    async fn health_check_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/path"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&server)
            .await;
        let err = make_client(&server).health_check().await.unwrap_err();
        assert!(matches!(
            err,
            OpenCodeError::ServerError { status: 503, .. }
        ));
    }

    #[tokio::test]
    async fn create_session_returns_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "ses_abc"})),
            )
            .mount(&server)
            .await;
        let id = make_client(&server)
            .create_session("/tmp/workspace")
            .await
            .unwrap();
        assert_eq!(id, "ses_abc");
    }

    #[tokio::test]
    async fn create_session_passes_directory_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session"))
            .and(header("x-opencode-directory", "/my/project"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "ses_dir_test"})),
            )
            .mount(&server)
            .await;
        make_client(&server)
            .create_session("/my/project")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn get_session_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/session/ses_abc"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "ses_abc"})),
            )
            .mount(&server)
            .await;
        let info = make_client(&server).get_session("ses_abc").await.unwrap();
        assert!(info.is_some());
    }

    #[tokio::test]
    async fn get_session_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/session/ses_missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let info = make_client(&server)
            .get_session("ses_missing")
            .await
            .unwrap();
        assert!(info.is_none());
    }

    #[tokio::test]
    async fn send_message_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/ses_abc/message"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "info": {"id": "msg_1", "role": "assistant"},
                "parts": [{"type": "text", "text": "Hello!"}]
            })))
            .mount(&server)
            .await;
        let resp = make_client(&server)
            .send_message("ses_abc", "Hi", "minimax", "MiniMax-M2.7-highspeed")
            .await
            .unwrap();
        assert_eq!(resp.text(), "Hello!");
    }

    #[tokio::test]
    async fn send_message_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/ses_abc/message"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&server)
            .await;
        let err = make_client(&server)
            .send_message("ses_abc", "Hi", "minimax", "MiniMax-M2.7-highspeed")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            OpenCodeError::ServerError { status: 500, .. }
        ));
    }

    #[tokio::test]
    async fn send_message_async_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/ses_abc/prompt_async"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        make_client(&server)
            .send_message_async("ses_abc", "Hi", "minimax", "MiniMax-M2.7-highspeed")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn abort_returns_true() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/ses_abc/abort"))
            .respond_with(ResponseTemplate::new(200).set_body_json(true))
            .mount(&server)
            .await;
        let aborted = make_client(&server).abort("ses_abc").await.unwrap();
        assert!(aborted);
    }

    #[tokio::test]
    async fn abort_returns_false() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/ses_abc/abort"))
            .respond_with(ResponseTemplate::new(200).set_body_json(false))
            .mount(&server)
            .await;
        let aborted = make_client(&server).abort("ses_abc").await.unwrap();
        assert!(!aborted);
    }

    #[tokio::test]
    async fn delete_session_ok() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/session/ses_abc"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        make_client(&server)
            .delete_session("ses_abc")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_session_not_found_is_ok() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/session/ses_gone"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        make_client(&server)
            .delete_session("ses_gone")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn basic_auth_header_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/path"))
            .and(header("authorization", "Basic b3BlbmNvZGU6c2VjcmV0")) // opencode:secret base64
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = OpenCodeClient {
            http: reqwest::Client::new(),
            base_url: server.uri(),
            password: Some("secret".to_string()),
        };
        client.health_check().await.unwrap();
    }
}
