//! Write the OpenCode server configuration file (`opencode.json`).
//!
//! Called once at daemon startup before spawning `opencode serve`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tracing::info;

use crate::config::OpenCodeConfig;

// ── Serialization structs (match confirmed OpenCode wire format) ─────────────

#[derive(Serialize)]
struct OpencodeJsonServer {
    port: u16,
    hostname: String,
}

#[derive(Serialize)]
struct OpencodeJsonProviderOptions {
    #[serde(rename = "apiKey")]
    api_key: String,
    #[serde(rename = "baseURL")]
    base_url: String,
}

#[derive(Serialize)]
struct OpencodeJsonProvider {
    npm: String,
    name: String,
    options: OpencodeJsonProviderOptions,
    models: HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct OpencodeJsonCompaction {
    auto: bool,
}

#[derive(Serialize)]
struct OpencodeJson {
    server: OpencodeJsonServer,
    provider: HashMap<String, OpencodeJsonProvider>,
    model: String,
    compaction: OpencodeJsonCompaction,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    instructions: Vec<String>,
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Write `opencode.json` to `{workspace_dir}/opencode/opencode.json`.
///
/// Returns the path to the written file on success.
///
/// # Errors
///
/// Returns an error if `api_key` is empty or any I/O operation fails.
pub async fn write_opencode_config(
    config: &OpenCodeConfig,
    api_key: &str,
    fallback_api_key: Option<&str>,
    workspace_dir: &Path,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context as _;

    let config_dir = workspace_dir.join("opencode");
    tokio::fs::create_dir_all(&config_dir)
        .await
        .with_context(|| format!("create opencode config dir: {}", config_dir.display()))?;

    // For built-in providers (openai, anthropic) that use OAuth from auth.json,
    // write a minimal config without custom provider block.
    let is_builtin = matches!(config.provider.as_str(), "openai" | "anthropic");

    // Include AGENTS.md if it exists in the opencode config dir.
    let agents_path = config_dir.join("AGENTS.md");
    let instructions: Vec<String> = if agents_path.exists() {
        vec!["AGENTS.md".to_string()]
    } else {
        vec![]
    };

    let json = if is_builtin {
        let mut val = serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "server": {
                "port": config.port,
                "hostname": &config.hostname,
            },
            "model": format!("{}/{}", config.provider, config.model),
            "compaction": { "auto": true },
        });
        if !instructions.is_empty() {
            val["instructions"] = serde_json::json!(instructions);
        }
        // Add fallback provider if configured
        if let (Some(fb_provider), Some(fb_model), Some(fb_base_url), Some(fb_key)) = (
            config.fallback_provider.as_deref(),
            config.fallback_model.as_deref(),
            config.fallback_base_url.as_deref(),
            fallback_api_key,
        ) {
            if !fb_key.is_empty() {
                let fb_display = provider_display_name(fb_provider);
                let mut fb_models = serde_json::Map::new();
                fb_models.insert(
                    fb_model.to_string(),
                    serde_json::Value::Object(serde_json::Map::default()),
                );
                val["provider"] = serde_json::json!({
                    fb_provider: {
                        "npm": "@ai-sdk/openai-compatible",
                        "name": fb_display,
                        "options": {
                            "apiKey": fb_key,
                            "baseURL": fb_base_url,
                        },
                        "models": fb_models,
                    }
                });
            }
        }
        serde_json::to_string_pretty(&val).context("serialize opencode.json")?
    } else {
        if api_key.is_empty() {
            anyhow::bail!(
                "OpenCode API key is empty — check [opencode].api_key_profile in config.toml"
            );
        }

        // Display name: "minimax" → "MiniMax", etc.
        let display_name = provider_display_name(&config.provider);

        // Build the models map: one entry per model configured
        let mut models: HashMap<String, serde_json::Value> = HashMap::new();
        models.insert(
            config.model.clone(),
            serde_json::Value::Object(serde_json::Map::default()),
        );

        // Build provider map
        let mut provider: HashMap<String, OpencodeJsonProvider> = HashMap::new();
        provider.insert(
            config.provider.clone(),
            OpencodeJsonProvider {
                npm: "@ai-sdk/openai-compatible".to_string(),
                name: display_name,
                options: OpencodeJsonProviderOptions {
                    api_key: api_key.to_string(),
                    base_url: config.base_url.clone(),
                },
                models,
            },
        );

        // Add fallback provider if configured
        if let (Some(fb_provider), Some(fb_model), Some(fb_base_url), Some(fb_key)) = (
            config.fallback_provider.as_deref(),
            config.fallback_model.as_deref(),
            config.fallback_base_url.as_deref(),
            fallback_api_key,
        ) {
            if !fb_key.is_empty() {
                let fb_display = provider_display_name(fb_provider);
                let mut fb_models: HashMap<String, serde_json::Value> = HashMap::new();
                fb_models.insert(
                    fb_model.to_string(),
                    serde_json::Value::Object(serde_json::Map::default()),
                );
                provider.insert(
                    fb_provider.to_string(),
                    OpencodeJsonProvider {
                        npm: "@ai-sdk/openai-compatible".to_string(),
                        name: fb_display,
                        options: OpencodeJsonProviderOptions {
                            api_key: fb_key.to_string(),
                            base_url: fb_base_url.to_string(),
                        },
                        models: fb_models,
                    },
                );
            }
        }

        serde_json::to_string_pretty(&OpencodeJson {
            server: OpencodeJsonServer {
                port: config.port,
                hostname: config.hostname.clone(),
            },
            provider,
            model: format!("{}/{}", config.provider, config.model),
            compaction: OpencodeJsonCompaction { auto: true },
            instructions,
        })
        .context("serialize opencode.json")?
    };

    // Atomic write: write to .tmp then rename
    let out_path = config_dir.join("opencode.json");
    let tmp_path = config_dir.join("opencode.json.tmp");
    tokio::fs::write(&tmp_path, &json)
        .await
        .with_context(|| format!("write {}", tmp_path.display()))?;
    tokio::fs::rename(&tmp_path, &out_path)
        .await
        .with_context(|| format!("rename {} → {}", tmp_path.display(), out_path.display()))?;

    info!(
        provider = %config.provider,
        model = %config.model,
        port = config.port,
        path = %out_path.display(),
        "wrote opencode.json"
    );

    Ok(out_path)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn provider_display_name(provider: &str) -> String {
    match provider {
        "minimax" => "MiniMax".to_string(),
        "anthropic" => "Anthropic".to_string(),
        "openai" => "OpenAI".to_string(),
        "google" | "gemini" => "Google".to_string(),
        "groq" => "Groq".to_string(),
        "moonshot" | "kimi" => "Moonshot AI".to_string(),
        other => {
            // Capitalize first letter
            let mut chars = other.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_config() -> OpenCodeConfig {
        OpenCodeConfig {
            enabled: true,
            port: 14096,
            hostname: "127.0.0.1".to_string(),
            provider: "minimax".to_string(),
            model: "MiniMax-M2.7-highspeed".to_string(),
            base_url: "https://api.minimax.chat/v1".to_string(),
            api_key_profile: None,
            history_inject_limit: 50,
            history_inject_max_chars: 50_000,
            idle_timeout_secs: 1800,
            fallback_provider: None,
            fallback_model: None,
            fallback_base_url: None,
            fallback_api_key_profile: None,
            stall_warn_secs: 30,
            stall_abort_secs: 120,
        }
    }

    #[tokio::test]
    async fn write_creates_valid_json() {
        let dir = tempdir().unwrap();
        let path = write_opencode_config(&test_config(), "test-key", None, dir.path())
            .await
            .unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(val["server"]["port"], 14096);
        assert_eq!(val["server"]["hostname"], "127.0.0.1");
        assert!(val["provider"]["minimax"].is_object());
        assert_eq!(val["model"], "minimax/MiniMax-M2.7-highspeed");
        assert_eq!(val["compaction"]["auto"], true);
    }

    #[tokio::test]
    async fn write_rejects_empty_key() {
        let dir = tempdir().unwrap();
        let result = write_opencode_config(&test_config(), "", None, dir.path()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[tokio::test]
    async fn write_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("nested").join("workspace");
        let result = write_opencode_config(&test_config(), "key", None, &nested).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn write_no_tmp_file_remains() {
        let dir = tempdir().unwrap();
        write_opencode_config(&test_config(), "key", None, dir.path())
            .await
            .unwrap();
        assert!(!dir
            .path()
            .join("opencode")
            .join("opencode.json.tmp")
            .exists());
    }

    #[tokio::test]
    async fn write_model_field_is_provider_slash_model() {
        let dir = tempdir().unwrap();
        let path = write_opencode_config(&test_config(), "key", None, dir.path())
            .await
            .unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(val["model"], "minimax/MiniMax-M2.7-highspeed");
    }

    #[tokio::test]
    async fn write_includes_fallback_provider() {
        let dir = tempdir().unwrap();
        let mut cfg = test_config();
        cfg.fallback_provider = Some("moonshot".to_string());
        cfg.fallback_model = Some("kimi-k2-0905-preview".to_string());
        cfg.fallback_base_url = Some("https://api.moonshot.cn/v1".to_string());
        let path = write_opencode_config(&cfg, "primary-key", Some("fallback-key"), dir.path())
            .await
            .unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(val["provider"]["minimax"].is_object());
        assert!(val["provider"]["moonshot"].is_object());
        assert_eq!(
            val["provider"]["moonshot"]["options"]["apiKey"],
            "fallback-key"
        );
        assert_eq!(
            val["provider"]["moonshot"]["options"]["baseURL"],
            "https://api.moonshot.cn/v1"
        );
    }

    #[test]
    fn provider_display_name_known() {
        assert_eq!(provider_display_name("minimax"), "MiniMax");
        assert_eq!(provider_display_name("anthropic"), "Anthropic");
        assert_eq!(provider_display_name("openai"), "OpenAI");
    }

    #[test]
    fn provider_display_name_unknown_capitalizes() {
        assert_eq!(provider_display_name("groq"), "Groq");
        assert_eq!(provider_display_name("custom"), "Custom");
    }

    #[test]
    fn provider_display_name_empty() {
        assert_eq!(provider_display_name(""), "");
    }
}
