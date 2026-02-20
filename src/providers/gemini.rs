//! Google Gemini provider with support for:
//! - Direct API key (`GEMINI_API_KEY` env var or config)
//! - Gemini CLI OAuth tokens (reuse existing ~/.gemini/ authentication)
//! - Google Cloud ADC (`GOOGLE_APPLICATION_CREDENTIALS`)

use crate::providers::traits::{ChatMessage, Provider};
use async_trait::async_trait;
use directories::UserDirs;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Gemini provider supporting multiple authentication methods.
///
/// For OAuth, supports multiple credential files (e.g. from separate Gemini CLI
/// installations). On 429/5xx errors the provider rotates to the next credential.
pub struct GeminiProvider {
    auth: Option<GeminiAuth>,
    /// Cloud AI Companion project ID, required for OAuth (cloudcode-pa) requests.
    /// Resolved via `loadCodeAssist` during warmup.
    project: std::sync::Mutex<Option<String>>,
    /// Discovered OAuth credential file paths for rotation on rate-limit errors.
    oauth_cred_paths: Vec<PathBuf>,
    /// Current index into `oauth_cred_paths`.
    oauth_index: std::sync::Mutex<usize>,
}

/// Resolved credential — the variant determines both the HTTP auth method
/// and the diagnostic label returned by `auth_source()`.
#[derive(Debug)]
enum GeminiAuth {
    /// Explicit API key from config: sent as `?key=` query parameter.
    ExplicitKey(String),
    /// API key from `GEMINI_API_KEY` env var: sent as `?key=`.
    EnvGeminiKey(String),
    /// API key from `GOOGLE_API_KEY` env var: sent as `?key=`.
    EnvGoogleKey(String),
    /// OAuth access token from Gemini CLI: sent as `Authorization: Bearer`.
    OAuthToken(String),
}

impl GeminiAuth {
    /// Whether this credential is an API key (sent as `?key=` query param).
    fn is_api_key(&self) -> bool {
        matches!(
            self,
            GeminiAuth::ExplicitKey(_) | GeminiAuth::EnvGeminiKey(_) | GeminiAuth::EnvGoogleKey(_)
        )
    }

    /// Whether this credential is an OAuth token from Gemini CLI.
    fn is_oauth(&self) -> bool {
        matches!(self, GeminiAuth::OAuthToken(_))
    }

    /// The raw credential string.
    fn credential(&self) -> &str {
        match self {
            GeminiAuth::ExplicitKey(s)
            | GeminiAuth::EnvGeminiKey(s)
            | GeminiAuth::EnvGoogleKey(s)
            | GeminiAuth::OAuthToken(s) => s,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// API REQUEST/RESPONSE TYPES
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, Clone)]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

/// Request envelope for the internal cloudcode-pa API.
/// OAuth tokens from Gemini CLI are scoped for this endpoint.
#[derive(Debug, Serialize)]
struct InternalGenerateContentEnvelope {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_prompt_id: Option<String>,
    request: InternalGenerateContentRequest,
}

/// Nested request payload for cloudcode-pa's code assist APIs.
#[derive(Debug, Serialize)]
struct InternalGenerateContentRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Debug, Serialize, Clone)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(Debug, Serialize, Clone)]
struct Part {
    text: String,
}

#[derive(Debug, Serialize, Clone)]
struct GenerationConfig {
    temperature: f64,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<ApiError>,
    #[serde(default)]
    response: Option<Box<GenerateContentResponse>>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Debug, Deserialize)]
struct CandidateContent {
    parts: Vec<ResponsePart>,
}

#[derive(Debug, Deserialize)]
struct ResponsePart {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: String,
}

impl GenerateContentResponse {
    /// cloudcode-pa wraps the actual response under `response`.
    fn into_effective_response(self) -> Self {
        match self {
            Self {
                response: Some(inner),
                ..
            } => *inner,
            other => other,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// GEMINI CLI TOKEN STRUCTURES
// ══════════════════════════════════════════════════════════════════════════════

/// OAuth token stored by Gemini CLI in `~/.gemini/oauth_creds.json`
#[derive(Debug, Deserialize, Serialize)]
struct GeminiCliOAuthCreds {
    access_token: Option<String>,
    refresh_token: Option<String>,
    scope: Option<String>,
    token_type: Option<String>,
    id_token: Option<serde_json::Value>,
    /// RFC-3339 expiry (older Gemini CLI versions).
    expiry: Option<String>,
    /// Epoch-ms expiry (current Gemini CLI).
    expiry_date: Option<u64>,
}

/// Gemini CLI OAuth client credentials — public values published by Google in
/// the Gemini CLI source.  Read from `~/.gemini/oauth_creds.json` client
/// fields, or override with `GEMINI_OAUTH_CLIENT_ID` /
/// `GEMINI_OAUTH_CLIENT_SECRET` env vars.
///
/// The default values are loaded from the first discovered credentials file
/// that contains `client_id`/`client_secret` keys. If no file exists, the
/// caller should set the env vars.
fn oauth_client_id() -> String {
    std::env::var("GEMINI_OAUTH_CLIENT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| load_oauth_client_field("client_id"))
        .unwrap_or_default()
}

fn oauth_client_secret() -> String {
    std::env::var("GEMINI_OAUTH_CLIENT_SECRET")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| load_oauth_client_field("client_secret"))
        .unwrap_or_default()
}

/// Read a field from the first discovered Gemini OAuth credentials file.
fn load_oauth_client_field(field: &str) -> Option<String> {
    let home = directories::UserDirs::new()?.home_dir().to_path_buf();
    let path = home.join(".gemini").join("oauth_creds.json");
    let data = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&data).ok()?;
    json.get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// Internal API endpoint used by Gemini CLI for OAuth users.
/// See: https://github.com/google-gemini/gemini-cli/issues/19200
const CLOUDCODE_PA_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal";

/// Public API endpoint for API key users.
const PUBLIC_API_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

impl GeminiProvider {
    /// Create a new Gemini provider.
    ///
    /// Authentication priority:
    /// 1. Explicit API key passed in
    /// 2. `GEMINI_API_KEY` environment variable
    /// 3. `GOOGLE_API_KEY` environment variable
    /// 4. Gemini CLI OAuth tokens (`~/.gemini/oauth_creds.json`)
    pub fn new(api_key: Option<&str>) -> Self {
        let oauth_cred_paths = Self::discover_oauth_cred_paths();

        let resolved_auth = api_key
            .and_then(Self::normalize_non_empty)
            .map(GeminiAuth::ExplicitKey)
            .or_else(|| Self::load_non_empty_env("GEMINI_API_KEY").map(GeminiAuth::EnvGeminiKey))
            .or_else(|| Self::load_non_empty_env("GOOGLE_API_KEY").map(GeminiAuth::EnvGoogleKey))
            .or_else(|| {
                Self::load_token_from_path(oauth_cred_paths.first()?).map(GeminiAuth::OAuthToken)
            });

        Self {
            auth: resolved_auth,
            project: std::sync::Mutex::new(None),
            oauth_cred_paths,
            oauth_index: std::sync::Mutex::new(0),
        }
    }

    fn normalize_non_empty(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn load_non_empty_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .and_then(|value| Self::normalize_non_empty(&value))
    }

    /// Discover all OAuth credential files from known Gemini CLI installations.
    ///
    /// Looks in `~/.gemini/oauth_creds.json` (default) plus any
    /// `~/.gemini-*-home/.gemini/oauth_creds.json` siblings (extra accounts
    /// set up via `HOME=~/.gemini-X-home gemini`).
    fn discover_oauth_cred_paths() -> Vec<PathBuf> {
        let home = match UserDirs::new() {
            Some(u) => u.home_dir().to_path_buf(),
            None => return Vec::new(),
        };

        let mut paths = Vec::new();

        // Primary: ~/.gemini/oauth_creds.json
        let primary = home.join(".gemini/oauth_creds.json");
        if primary.exists() {
            paths.push(primary);
        }

        // Extra accounts: ~/.gemini-*-home/.gemini/oauth_creds.json
        if let Ok(entries) = std::fs::read_dir(&home) {
            let mut extras: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with(".gemini-") && name.ends_with("-home") {
                        let p = e.path().join(".gemini/oauth_creds.json");
                        if p.exists() {
                            return Some(p);
                        }
                    }
                    None
                })
                .collect();
            extras.sort();
            paths.extend(extras);
        }

        paths
    }

    /// Load an OAuth access token from a specific credential file.
    /// If the access token is expired but a refresh token is present,
    /// automatically refreshes and persists the new token.
    fn load_token_from_path(path: &PathBuf) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let creds: GeminiCliOAuthCreds = serde_json::from_str(&content).ok()?;

        let expired = Self::is_token_expired(&creds);

        if expired {
            // Try to refresh using the refresh_token.
            if let Some(ref rt) = creds.refresh_token {
                tracing::info!(
                    "Gemini OAuth token expired in {}, refreshing…",
                    path.display()
                );
                return Self::refresh_and_persist(rt, path);
            }
            tracing::warn!(
                "Gemini CLI OAuth token expired in {} — no refresh_token, re-run `gemini`",
                path.display()
            );
            return None;
        }

        creds
            .access_token
            .and_then(|token| Self::normalize_non_empty(&token))
    }

    fn is_token_expired(creds: &GeminiCliOAuthCreds) -> bool {
        if let Some(expiry_ms) = creds.expiry_date {
            let now_ms = chrono::Utc::now().timestamp_millis() as u64;
            // Refresh 60s before actual expiry to avoid races.
            return expiry_ms.saturating_sub(60_000) < now_ms;
        }
        if let Some(ref expiry) = creds.expiry {
            if let Ok(expiry_time) = chrono::DateTime::parse_from_rfc3339(expiry) {
                return expiry_time < chrono::Utc::now() + chrono::Duration::seconds(60);
            }
        }
        false
    }

    /// Use the refresh_token to obtain a new access_token, persist back to file.
    fn refresh_and_persist(refresh_token: &str, creds_path: &PathBuf) -> Option<String> {
        // Synchronous HTTP — called from non-async context during provider init.
        let client_id = oauth_client_id();
        let client_secret = oauth_client_secret();
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .ok()?;

        if !resp.status().is_success() {
            tracing::warn!(
                "Gemini OAuth refresh failed ({}): {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
            return None;
        }

        let body: serde_json::Value = resp.json().ok()?;
        let new_access_token = body.get("access_token")?.as_str()?;
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(3600);
        let new_expiry_ms = chrono::Utc::now().timestamp_millis() as u64 + expires_in * 1000;

        // Re-read and update the creds file to preserve all fields.
        if let Ok(content) = std::fs::read_to_string(creds_path) {
            if let Ok(mut creds) = serde_json::from_str::<GeminiCliOAuthCreds>(&content) {
                creds.access_token = Some(new_access_token.to_string());
                creds.expiry_date = Some(new_expiry_ms);
                // Persist — best-effort, don't fail if write fails.
                if let Ok(json) = serde_json::to_string_pretty(&creds) {
                    let _ = std::fs::write(creds_path, json);
                }
            }
        }

        tracing::info!("Gemini OAuth: refreshed token, expires in {expires_in}s");
        Some(new_access_token.to_string())
    }

    /// Check if Gemini CLI is configured and has valid credentials
    pub fn has_cli_credentials() -> bool {
        !Self::discover_oauth_cred_paths().is_empty()
    }

    /// Check if any Gemini authentication is available
    pub fn has_any_auth() -> bool {
        Self::load_non_empty_env("GEMINI_API_KEY").is_some()
            || Self::load_non_empty_env("GOOGLE_API_KEY").is_some()
            || Self::has_cli_credentials()
    }

    /// Get authentication source description for diagnostics.
    /// Uses the stored enum variant — no env var re-reading at call time.
    pub fn auth_source(&self) -> &'static str {
        match self.auth.as_ref() {
            Some(GeminiAuth::ExplicitKey(_)) => "config",
            Some(GeminiAuth::EnvGeminiKey(_)) => "GEMINI_API_KEY env var",
            Some(GeminiAuth::EnvGoogleKey(_)) => "GOOGLE_API_KEY env var",
            Some(GeminiAuth::OAuthToken(_)) => "Gemini CLI OAuth",
            None => "none",
        }
    }

    fn format_model_name(model: &str) -> String {
        if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        }
    }

    fn format_internal_model_name(model: &str) -> String {
        model.strip_prefix("models/").unwrap_or(model).to_string()
    }

    /// Build the API URL based on auth type.
    ///
    /// - API key users → public `generativelanguage.googleapis.com/v1beta`
    /// - OAuth users → internal `cloudcode-pa.googleapis.com/v1internal`
    ///
    /// The Gemini CLI OAuth tokens are scoped for the internal Code Assist API,
    /// not the public API. Sending them to the public endpoint results in
    /// "400 Bad Request: API key not valid" errors.
    /// See: https://github.com/google-gemini/gemini-cli/issues/19200
    fn build_generate_content_url(model: &str, auth: &GeminiAuth) -> String {
        match auth {
            GeminiAuth::OAuthToken(_) => {
                // OAuth tokens from Gemini CLI are scoped for the internal
                // Code Assist API. The model is passed in the request body,
                // not the URL path.
                format!("{CLOUDCODE_PA_ENDPOINT}:generateContent")
            }
            _ => {
                let model_name = Self::format_model_name(model);
                let base_url = format!("{PUBLIC_API_ENDPOINT}/{model_name}:generateContent");

                if auth.is_api_key() {
                    format!("{base_url}?key={}", auth.credential())
                } else {
                    base_url
                }
            }
        }
    }

    /// Resolve the Cloud AI Companion project ID via `loadCodeAssist`.
    async fn resolve_oauth_project(&self, auth: &GeminiAuth) -> anyhow::Result<()> {
        let resp = self
            .http_client()
            .post(format!("{CLOUDCODE_PA_ENDPOINT}:loadCodeAssist"))
            .bearer_auth(auth.credential())
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?;
        let body: serde_json::Value = resp.json().await?;
        if let Some(pid) = body.get("cloudaicompanionProject").and_then(|v| v.as_str()) {
            if let Ok(mut guard) = self.project.lock() {
                *guard = Some(pid.to_string());
            }
            tracing::info!("Gemini OAuth: resolved project {pid}");
        } else {
            tracing::warn!(
                "Gemini OAuth: loadCodeAssist did not return a project ID; \
                 API calls may fail"
            );
        }
        Ok(())
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.gemini", 120, 10)
    }

    fn build_generate_content_request(
        &self,
        auth: &GeminiAuth,
        url: &str,
        request: &GenerateContentRequest,
        model: &str,
    ) -> reqwest::RequestBuilder {
        let client = self.http_client();
        match auth {
            GeminiAuth::OAuthToken(token) => {
                let project = self.project.lock().ok().and_then(|g| g.clone());
                let envelope = InternalGenerateContentEnvelope {
                    model: Self::format_internal_model_name(model),
                    project,
                    user_prompt_id: None,
                    request: InternalGenerateContentRequest {
                        contents: request.contents.clone(),
                        system_instruction: request.system_instruction.clone(),
                        generation_config: request.generation_config.clone(),
                    },
                };
                client.post(url).json(&envelope).bearer_auth(token)
            }
            _ => client.post(url).json(request),
        }
    }

    fn resolve_oauth_project_id() -> Option<String> {
        for key in [
            "GEMINI_CODE_ASSIST_PROJECT",
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_PROJECT_ID",
        ] {
            if let Ok(value) = std::env::var(key) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
}

impl GeminiProvider {
    /// Rotate to the next OAuth credential file. Returns `true` if a new
    /// token was loaded successfully, `false` if no more credentials to try.
    fn rotate_oauth(&self) -> bool {
        if self.oauth_cred_paths.len() <= 1 {
            return false;
        }

        let mut idx = match self.oauth_index.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };

        let start = *idx;
        loop {
            let next = (*idx + 1) % self.oauth_cred_paths.len();
            *idx = next;

            if next == start {
                return false; // wrapped around — all exhausted
            }

            if Self::load_token_from_path(&self.oauth_cred_paths[next]).is_some() {
                tracing::info!(
                    "Gemini OAuth: rotated to credential {}",
                    self.oauth_cred_paths[next].display()
                );
                drop(idx);
                if let Ok(mut proj) = self.project.lock() {
                    *proj = None;
                }
                return true;
            }
        }
    }

    /// Get the current OAuth token based on oauth_index, re-reading from disk.
    fn current_oauth_token(&self) -> Option<String> {
        let idx = self.oauth_index.lock().ok()?;
        let path = self.oauth_cred_paths.get(*idx)?;
        Self::load_token_from_path(path)
    }

    async fn send_generate_content(
        &self,
        contents: Vec<Content>,
        system_instruction: Option<Content>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Gemini API key not found. Options:\n\
                 1. Set GEMINI_API_KEY env var\n\
                 2. Run `gemini` CLI to authenticate (tokens will be reused)\n\
                 3. Get an API key from https://aistudio.google.com/app/apikey\n\
                 4. Run `zeroclaw onboard` to configure"
            )
        })?;

        // For non-OAuth auth, use the simple path (no rotation).
        if !auth.is_oauth() {
            return self
                .send_generate_content_once(
                    &contents,
                    &system_instruction,
                    model,
                    temperature,
                    auth,
                )
                .await;
        }

        // OAuth path with rotation on 429/5xx.
        // First, try with the current token (possibly re-read from disk for freshness).
        let fresh_token = self
            .current_oauth_token()
            .unwrap_or_else(|| auth.credential().to_string());
        let current_auth = GeminiAuth::OAuthToken(fresh_token);

        match self
            .send_generate_content_once(
                &contents,
                &system_instruction,
                model,
                temperature,
                &current_auth,
            )
            .await
        {
            Ok(result) => Ok(result),
            Err(err) => {
                let err_str = err.to_string();
                let is_retryable = err_str.contains("429")
                    || err_str.contains("RESOURCE_EXHAUSTED")
                    || err_str.contains("500")
                    || err_str.contains("503");

                if is_retryable && self.rotate_oauth() {
                    tracing::warn!("Gemini: retrying with rotated OAuth credential after: {err}");
                    let next_token = self
                        .current_oauth_token()
                        .ok_or_else(|| anyhow::anyhow!("No valid OAuth token after rotation"))?;
                    let next_auth = GeminiAuth::OAuthToken(next_token);
                    self.send_generate_content_once(
                        &contents,
                        &system_instruction,
                        model,
                        temperature,
                        &next_auth,
                    )
                    .await
                } else {
                    Err(err)
                }
            }
        }
    }

    async fn send_generate_content_once(
        &self,
        contents: &[Content],
        system_instruction: &Option<Content>,
        model: &str,
        temperature: f64,
        auth: &GeminiAuth,
    ) -> anyhow::Result<String> {
        // Lazy-resolve cloudcode-pa project ID on first OAuth request.
        if auth.is_oauth() {
            let needs_resolve = self.project.lock().ok().map_or(true, |g| g.is_none());
            if needs_resolve {
                self.resolve_oauth_project(auth).await?;
            }
        }

        let request = GenerateContentRequest {
            contents: contents.to_vec(),
            system_instruction: system_instruction.clone(),
            generation_config: GenerationConfig {
                temperature,
                max_output_tokens: 8192,
            },
        };

        let url = Self::build_generate_content_url(model, auth);

        let response = self
            .build_generate_content_request(auth, &url, &request, model)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error ({status}): {error_text}");
        }

        let result: GenerateContentResponse = response.json().await?;
        if let Some(err) = &result.error {
            anyhow::bail!("Gemini API error: {}", err.message);
        }
        let result = result.into_effective_response();
        if let Some(err) = result.error {
            anyhow::bail!("Gemini API error: {}", err.message);
        }

        result
            .candidates
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.content.parts.into_iter().next())
            .and_then(|p| p.text)
            .ok_or_else(|| anyhow::anyhow!("No response from Gemini"))
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let system_instruction = system_prompt.map(|sys| Content {
            role: None,
            parts: vec![Part {
                text: sys.to_string(),
            }],
        });

        let contents = vec![Content {
            role: Some("user".to_string()),
            parts: vec![Part {
                text: message.to_string(),
            }],
        }];

        self.send_generate_content(contents, system_instruction, model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut contents: Vec<Content> = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    system_parts.push(&msg.content);
                }
                "user" => {
                    contents.push(Content {
                        role: Some("user".to_string()),
                        parts: vec![Part {
                            text: msg.content.clone(),
                        }],
                    });
                }
                "assistant" => {
                    // Gemini API uses "model" role instead of "assistant"
                    contents.push(Content {
                        role: Some("model".to_string()),
                        parts: vec![Part {
                            text: msg.content.clone(),
                        }],
                    });
                }
                _ => {}
            }
        }

        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            Some(Content {
                role: None,
                parts: vec![Part {
                    text: system_parts.join("\n\n"),
                }],
            })
        };

        self.send_generate_content(contents, system_instruction, model, temperature)
            .await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let auth = match self.auth.as_ref() {
            Some(a) => a,
            None => return Ok(()),
        };

        if auth.is_oauth() {
            // Use fresh token from disk (may have been auto-refreshed).
            let token = self
                .current_oauth_token()
                .unwrap_or_else(|| auth.credential().to_string());
            let fresh = GeminiAuth::OAuthToken(token);
            return self.resolve_oauth_project(&fresh).await;
        }

        let url = if auth.is_api_key() {
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models?key={}",
                auth.credential()
            )
        } else {
            "https://generativelanguage.googleapis.com/v1beta/models".to_string()
        };

        self.http_client()
            .get(&url)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    fn test_provider(auth: Option<GeminiAuth>) -> GeminiProvider {
        GeminiProvider {
            auth,
            project: std::sync::Mutex::new(None),
            oauth_cred_paths: Vec::new(),
            oauth_index: std::sync::Mutex::new(0),
        }
    }

    #[test]
    fn normalize_non_empty_trims_and_filters() {
        assert_eq!(
            GeminiProvider::normalize_non_empty(" value "),
            Some("value".into())
        );
        assert_eq!(GeminiProvider::normalize_non_empty(""), None);
        assert_eq!(GeminiProvider::normalize_non_empty(" \t\n"), None);
    }

    #[test]
    fn provider_creates_without_key() {
        let provider = GeminiProvider::new(None);
        // May pick up env vars; just verify it doesn't panic
        let _ = provider.auth_source();
    }

    #[test]
    fn provider_creates_with_key() {
        let provider = GeminiProvider::new(Some("test-api-key"));
        assert!(matches!(
            provider.auth,
            Some(GeminiAuth::ExplicitKey(ref key)) if key == "test-api-key"
        ));
    }

    #[test]
    fn provider_rejects_empty_key() {
        let provider = GeminiProvider::new(Some(""));
        assert!(!matches!(provider.auth, Some(GeminiAuth::ExplicitKey(_))));
    }

    #[test]
    fn auth_source_explicit_key() {
        let provider = test_provider(Some(GeminiAuth::ExplicitKey("key".into())));
        assert_eq!(provider.auth_source(), "config");
    }

    #[test]
    fn auth_source_none_without_credentials() {
        let provider = test_provider(None);
        assert_eq!(provider.auth_source(), "none");
    }

    #[test]
    fn auth_source_oauth() {
        let provider = test_provider(Some(GeminiAuth::OAuthToken("ya29.mock".into())));
        assert_eq!(provider.auth_source(), "Gemini CLI OAuth");
    }

    #[test]
    fn model_name_formatting() {
        assert_eq!(
            GeminiProvider::format_model_name("gemini-2.0-flash"),
            "models/gemini-2.0-flash"
        );
        assert_eq!(
            GeminiProvider::format_model_name("models/gemini-1.5-pro"),
            "models/gemini-1.5-pro"
        );
        assert_eq!(
            GeminiProvider::format_internal_model_name("models/gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
        assert_eq!(
            GeminiProvider::format_internal_model_name("gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
    }

    #[test]
    fn api_key_url_includes_key_query_param() {
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.contains(":generateContent?key=api-key-123"));
    }

    #[test]
    fn oauth_url_uses_internal_endpoint() {
        let auth = GeminiAuth::OAuthToken("ya29.test-token".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.starts_with("https://cloudcode-pa.googleapis.com/v1internal"));
        assert!(url.ends_with(":generateContent"));
        assert!(!url.contains("generativelanguage.googleapis.com"));
        assert!(!url.contains("?key="));
    }

    #[test]
    fn api_key_url_uses_public_endpoint() {
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.contains("generativelanguage.googleapis.com/v1beta"));
        assert!(url.contains("models/gemini-2.0-flash"));
    }

    #[test]
    fn oauth_request_uses_bearer_auth_header() {
        let provider = test_provider(Some(GeminiAuth::OAuthToken("ya29.mock-token".into())));
        let auth = GeminiAuth::OAuthToken("ya29.mock-token".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: "hello".into(),
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
        };

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "gemini-2.0-flash")
            .build()
            .unwrap();

        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|h| h.to_str().ok()),
            Some("Bearer ya29.mock-token")
        );
    }

    #[test]
    fn oauth_request_wraps_payload_in_request_envelope() {
        let provider = test_provider(Some(GeminiAuth::OAuthToken("ya29.mock-token".into())));
        let auth = GeminiAuth::OAuthToken("ya29.mock-token".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: "hello".into(),
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
        };

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "models/gemini-2.0-flash")
            .build()
            .unwrap();

        let payload = request
            .body()
            .and_then(|b| b.as_bytes())
            .expect("json request body should be bytes");
        let json: serde_json::Value = serde_json::from_slice(payload).unwrap();

        assert_eq!(json["model"], "gemini-2.0-flash");
        assert!(json.get("generationConfig").is_none());
        assert!(json.get("request").is_some());
        assert!(json["request"].get("generationConfig").is_some());
    }

    #[test]
    fn api_key_request_does_not_set_bearer_header() {
        let provider = test_provider(Some(GeminiAuth::ExplicitKey("api-key-123".into())));
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: "hello".into(),
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
        };

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "gemini-2.0-flash")
            .build()
            .unwrap();

        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn request_serialization() {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: "Hello".to_string(),
                }],
            }],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part {
                    text: "You are helpful".to_string(),
                }],
            }),
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"text\":\"Hello\""));
        assert!(json.contains("\"systemInstruction\""));
        assert!(!json.contains("\"system_instruction\""));
        assert!(json.contains("\"temperature\":0.7"));
        assert!(json.contains("\"maxOutputTokens\":8192"));
    }

    #[test]
    fn internal_request_includes_model() {
        let request = InternalGenerateContentEnvelope {
            model: "gemini-3-pro-preview".to_string(),
            project: None,
            user_prompt_id: Some("prompt-123".to_string()),
            request: InternalGenerateContentRequest {
                contents: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: "Hello".to_string(),
                    }],
                }],
                system_instruction: None,
                generation_config: GenerationConfig {
                    temperature: 0.7,
                    max_output_tokens: 8192,
                },
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"model\":\"gemini-3-pro-preview\""));
        assert!(json.contains("\"request\""));
        assert!(json.contains("\"generationConfig\""));
        assert!(json.contains("\"maxOutputTokens\":8192"));
        assert!(json.contains("\"user_prompt_id\":\"prompt-123\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"temperature\":0.7"));
    }

    #[test]
    fn response_deserialization() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello there!"}]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(response.candidates.is_some());
        let text = response
            .candidates
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .content
            .parts
            .into_iter()
            .next()
            .unwrap()
            .text;
        assert_eq!(text, Some("Hello there!".to_string()));
    }

    #[test]
    fn error_response_deserialization() {
        let json = r#"{
            "error": {
                "message": "Invalid API key"
            }
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().message, "Invalid API key");
    }

    #[test]
    fn internal_response_deserialization() {
        let json = r#"{
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{"text": "Hello from internal"}]
                    }
                }]
            }
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let text = response
            .into_effective_response()
            .candidates
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .content
            .parts
            .into_iter()
            .next()
            .unwrap()
            .text;
        assert_eq!(text, Some("Hello from internal".to_string()));
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let provider = test_provider(None);
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn warmup_oauth_calls_load_code_assist() {
        // With a fake token the loadCodeAssist call will fail (401/403),
        // so warmup should return an error rather than silently succeed.
        let provider = test_provider(Some(GeminiAuth::OAuthToken("ya29.mock-token".into())));
        let result = provider.warmup().await;
        assert!(
            result.is_err(),
            "warmup with invalid OAuth token should fail"
        );
    }

    #[test]
    fn rotate_oauth_with_no_paths_returns_false() {
        let provider = test_provider(Some(GeminiAuth::OAuthToken("ya29.mock".into())));
        assert!(!provider.rotate_oauth());
    }

    #[test]
    fn rotate_oauth_with_single_path_returns_false() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::OAuthToken("ya29.mock".into())),
            project: std::sync::Mutex::new(None),
            oauth_cred_paths: vec![PathBuf::from("/tmp/fake_cred.json")],
            oauth_index: std::sync::Mutex::new(0),
        };
        assert!(!provider.rotate_oauth());
    }

    #[test]
    fn discover_oauth_cred_paths_includes_primary() {
        let paths = GeminiProvider::discover_oauth_cred_paths();
        // On this machine, ~/.gemini/oauth_creds.json exists.
        // In CI it may not, so just check the function doesn't panic.
        let home = std::env::var("HOME").unwrap_or_default();
        if std::path::Path::new(&format!("{home}/.gemini/oauth_creds.json")).exists() {
            assert!(!paths.is_empty());
        }
    }
}
