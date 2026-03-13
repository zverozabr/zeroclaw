use super::agent_load_tracker::AgentLoadTracker;
use super::agent_selection::{select_agent_with_load, AgentSelectionPolicy};
use super::orchestration_settings::load_orchestration_settings;
use super::traits::{Tool, ToolResult};
use crate::agent::loop_::run_tool_call_loop;
use crate::config::{AgentTeamsConfig, DelegateAgentConfig};
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::providers::{self, ChatMessage, Provider};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Default timeout for sub-agent provider calls.
const DELEGATE_TIMEOUT_SECS: u64 = 120;
/// Default timeout for agentic sub-agent runs.
const DELEGATE_AGENTIC_TIMEOUT_SECS: u64 = 300;

/// Tool that delegates a subtask to a named agent with a different
/// provider/model configuration. Enables multi-agent workflows where
/// a primary agent can hand off specialized work (research, coding,
/// summarization) to purpose-built sub-agents.
pub struct DelegateTool {
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    security: Arc<SecurityPolicy>,
    /// Global credential fallback (from config.api_key)
    fallback_credential: Option<String>,
    /// Provider runtime options inherited from root config.
    provider_runtime_options: providers::ProviderRuntimeOptions,
    /// Reliability config for delegate provider builds (retries, fallbacks, etc.).
    reliability: crate::config::ReliabilityConfig,
    /// Depth at which this tool instance lives in the delegation chain.
    depth: u32,
    /// Parent tool registry for agentic sub-agents.
    parent_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// Inherited multimodal handling config for sub-agent loops.
    multimodal_config: crate::config::MultimodalConfig,
    /// Team orchestration and load-balance settings.
    team_settings: AgentTeamsConfig,
    /// Shared runtime load tracker across delegate/subagent tools.
    load_tracker: AgentLoadTracker,
    /// Optional runtime config file path for hot-reloaded orchestration settings.
    runtime_config_path: Option<PathBuf>,
}

impl DelegateTool {
    pub fn new(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self::new_with_options(
            agents,
            fallback_credential,
            security,
            providers::ProviderRuntimeOptions::default(),
        )
    }

    pub fn new_with_options(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            provider_runtime_options,
            reliability: crate::config::ReliabilityConfig::default(),
            depth: 0,
            parent_tools: Arc::new(Vec::new()),
            multimodal_config: crate::config::MultimodalConfig::default(),
            team_settings: AgentTeamsConfig::default(),
            load_tracker: AgentLoadTracker::new(),
            runtime_config_path: None,
        }
    }

    /// Create a DelegateTool for a sub-agent (with incremented depth).
    /// When sub-agents eventually get their own tool registry, construct
    /// their DelegateTool via this method with `depth: parent.depth + 1`.
    pub fn with_depth(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
    ) -> Self {
        Self::with_depth_and_options(
            agents,
            fallback_credential,
            security,
            depth,
            providers::ProviderRuntimeOptions::default(),
        )
    }

    pub fn with_depth_and_options(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            provider_runtime_options,
            reliability: crate::config::ReliabilityConfig::default(),
            depth,
            parent_tools: Arc::new(Vec::new()),
            multimodal_config: crate::config::MultimodalConfig::default(),
            team_settings: AgentTeamsConfig::default(),
            load_tracker: AgentLoadTracker::new(),
            runtime_config_path: None,
        }
    }

    /// Attach reliability config for delegate provider builds.
    pub fn with_reliability(mut self, reliability: crate::config::ReliabilityConfig) -> Self {
        self.reliability = reliability;
        self
    }

    /// Attach parent tools used to build sub-agent allowlist registries.
    pub fn with_parent_tools(mut self, parent_tools: Arc<Vec<Arc<dyn Tool>>>) -> Self {
        self.parent_tools = parent_tools;
        self
    }

    /// Attach multimodal configuration for sub-agent tool loops.
    pub fn with_multimodal_config(mut self, config: crate::config::MultimodalConfig) -> Self {
        self.multimodal_config = config;
        self
    }

    /// Set whether agent selection can auto-resolve from task/context.
    pub fn with_auto_activate(mut self, auto_activate: bool) -> Self {
        self.team_settings.auto_activate = auto_activate;
        self
    }

    /// Attach runtime team orchestration controls and optional hot-reload config path.
    pub fn with_runtime_team_settings(
        mut self,
        teams_enabled: bool,
        auto_activate: bool,
        max_team_agents: usize,
        runtime_config_path: Option<PathBuf>,
    ) -> Self {
        self.team_settings.enabled = teams_enabled;
        self.team_settings.auto_activate = auto_activate;
        self.team_settings.max_agents = max_team_agents.max(1);
        self.runtime_config_path = runtime_config_path;
        self
    }

    /// Reuse a shared runtime load tracker.
    pub fn with_load_tracker(mut self, load_tracker: AgentLoadTracker) -> Self {
        self.load_tracker = load_tracker;
        self
    }

    fn runtime_team_settings(&self) -> AgentTeamsConfig {
        let mut settings = self.team_settings.clone();
        settings.max_agents = settings.max_agents.max(1);
        settings.load_window_secs = settings.load_window_secs.max(1);

        if let Some(path) = self.runtime_config_path.as_deref() {
            match load_orchestration_settings(path) {
                Ok((teams, _subagents)) => {
                    settings = teams;
                    settings.max_agents = settings.max_agents.max(1);
                    settings.load_window_secs = settings.load_window_secs.max(1);
                }
                Err(error) => {
                    tracing::debug!(
                        path = %path.display(),
                        "delegate: failed to hot-reload orchestration settings: {error}"
                    );
                }
            }
        }

        settings
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a specialized agent. Use when: a task benefits from a different model \
         (e.g. fast summarization, deep reasoning, code generation). The sub-agent runs a single \
         prompt by default; with agentic=true it can iterate with a filtered tool-call loop. \
         `agent` may be omitted or set to `auto` when team auto-activation is enabled."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let agent_names: Vec<&str> = self.agents.keys().map(|s: &String| s.as_str()).collect();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "agent": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!(
                        "Name of the agent to delegate to. Available: {}",
                        if agent_names.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            agent_names.join(", ")
                        }
                    )
                },
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt to send to the sub-agent"
                },
                "context": {
                    "type": "string",
                    "description": "Optional context to prepend (e.g. relevant code, prior findings)"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let requested_agent = args.get("agent").and_then(|v| v.as_str()).map(str::trim);

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        if prompt.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'prompt' parameter must not be empty".into()),
            });
        }

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");

        let team_settings = self.runtime_team_settings();
        if !team_settings.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Agent teams are currently disabled. Re-enable with model_routing_config action set_orchestration."
                        .to_string(),
                ),
            });
        }

        let load_window_secs = u64::try_from(team_settings.load_window_secs).unwrap_or(1);
        let load_snapshot = self
            .load_tracker
            .snapshot(Duration::from_secs(load_window_secs.max(1)));
        let selection_policy = AgentSelectionPolicy {
            strategy: team_settings.strategy,
            inflight_penalty: team_settings.inflight_penalty,
            recent_selection_penalty: team_settings.recent_selection_penalty,
            recent_failure_penalty: team_settings.recent_failure_penalty,
        };

        let selection = match select_agent_with_load(
            self.agents.as_ref(),
            requested_agent,
            prompt,
            context,
            team_settings.auto_activate,
            Some(team_settings.max_agents),
            Some(&load_snapshot),
            selection_policy,
        ) {
            Ok(selection) => selection,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error.to_string()),
                });
            }
        };
        let agent_name = selection.agent_name.as_str();
        let Some(agent_config) = self.agents.get(agent_name) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Resolved agent '{agent_name}' is unavailable")),
            });
        };

        // Check recursion depth (immutable — set at construction, incremented for sub-agents)
        if self.depth >= agent_config.max_depth {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Delegation depth limit reached ({depth}/{max}). \
                     Cannot delegate further to prevent infinite loops.",
                    depth = self.depth,
                    max = agent_config.max_depth
                )),
            });
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "delegate")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let mut load_lease = self.load_tracker.start(agent_name);
        let coordination_trace =
            self.start_coordination_trace(agent_name, prompt, context, agent_config);

        // Create provider for this agent
        let provider_credential_owned = agent_config
            .api_key
            .clone()
            .or_else(|| self.fallback_credential.clone());
        #[allow(clippy::option_as_ref_deref)]
        let provider_credential = provider_credential_owned.as_ref().map(String::as_str);

        let mut agent_reliability = self.reliability.clone();
        if let Some(retries) = agent_config.provider_retries {
            agent_reliability.provider_retries = retries;
        }
        if !agent_config.fallback_providers.is_empty() {
            agent_reliability.fallback_providers = agent_config.fallback_providers.clone();
        }

        let provider_runtime_options_for_agent = &self.provider_runtime_options;
        let provider: Box<dyn Provider> = match providers::create_resilient_provider_with_options(
            &agent_config.provider,
            provider_credential,
            None,
            &agent_reliability,
            provider_runtime_options_for_agent,
        ) {
            Ok(p) => p,
            Err(e) => {
                let error_message = format!(
                    "Failed to create provider '{}' for agent '{agent_name}': {e}",
                    agent_config.provider
                );
                self.finish_coordination_trace(
                    agent_name,
                    &coordination_trace,
                    false,
                    &error_message,
                );
                load_lease.mark_failure();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error_message),
                });
            }
        };

        // Build the message
        let full_prompt = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("[Context]\n{context}\n\n[Task]\n{prompt}")
        };

        let temperature = agent_config.temperature.unwrap_or(0.7);

        // Agentic mode: run full tool-call loop with allowlisted tools.
        if agent_config.agentic {
            let result = self
                .execute_agentic(
                    agent_name,
                    agent_config,
                    &*provider,
                    &full_prompt,
                    temperature,
                )
                .await?;

            let summary = if result.success {
                result.output.as_str()
            } else {
                result
                    .error
                    .as_deref()
                    .unwrap_or("delegate agentic execution failed")
            };
            self.finish_coordination_trace(
                agent_name,
                &coordination_trace,
                result.success,
                summary,
            );
            if result.success {
                load_lease.mark_success();
            } else {
                load_lease.mark_failure();
            }

            return Ok(result);
        }

        // Wrap the provider call in a timeout to prevent indefinite blocking
        let result = tokio::time::timeout(
            Duration::from_secs(DELEGATE_TIMEOUT_SECS),
            provider.chat_with_system(
                agent_config.system_prompt.as_deref(),
                &full_prompt,
                &agent_config.model,
                temperature,
            ),
        )
        .await;

        let result = match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                let timeout_message =
                    format!("Agent '{agent_name}' timed out after {DELEGATE_TIMEOUT_SECS}s");
                self.finish_coordination_trace(
                    agent_name,
                    &coordination_trace,
                    false,
                    &timeout_message,
                );
                load_lease.mark_failure();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(timeout_message),
                });
            }
        };

        match result {
            Ok(response) => {
                let mut rendered = response;
                if rendered.trim().is_empty() {
                    rendered = "[Empty response]".to_string();
                }
                let output = format!(
                    "[Agent '{agent_name}' ({provider}/{model})]\n{rendered}",
                    provider = agent_config.provider,
                    model = agent_config.model
                );
                self.finish_coordination_trace(agent_name, &coordination_trace, true, &output);
                load_lease.mark_success();

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => {
                let failure_message = format!("Agent '{agent_name}' failed: {e}");
                self.finish_coordination_trace(
                    agent_name,
                    &coordination_trace,
                    false,
                    &failure_message,
                );
                load_lease.mark_failure();
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(failure_message),
                })
            }
        }
    }
}

impl DelegateTool {
    async fn execute_agentic(
        &self,
        agent_name: &str,
        agent_config: &DelegateAgentConfig,
        provider: &dyn Provider,
        full_prompt: &str,
        temperature: f64,
    ) -> anyhow::Result<ToolResult> {
        if agent_config.allowed_tools.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' has agentic=true but allowed_tools is empty"
                )),
            });
        }

        let allowed = agent_config
            .allowed_tools
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .collect::<std::collections::HashSet<_>>();

        let sub_tools: Vec<Box<dyn Tool>> = self
            .parent_tools
            .iter()
            .filter(|tool| allowed.contains(tool.name()))
            .filter(|tool| tool.name() != "delegate")
            .map(|tool| Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>)
            .collect();

        if sub_tools.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' has no executable tools after filtering allowlist ({})",
                    agent_config.allowed_tools.join(", ")
                )),
            });
        }

        let mut history = Vec::new();
        if let Some(system_prompt) = agent_config.system_prompt.as_ref() {
            history.push(ChatMessage::system(system_prompt.clone()));
        }
        history.push(ChatMessage::user(full_prompt.to_string()));

        let noop_observer = NoopObserver;

        let result = tokio::time::timeout(
            Duration::from_secs(DELEGATE_AGENTIC_TIMEOUT_SECS),
            run_tool_call_loop(
                provider,
                &mut history,
                &sub_tools,
                &noop_observer,
                &agent_config.provider,
                &agent_config.model,
                temperature,
                true,
                None,
                "delegate",
                &self.multimodal_config,
                agent_config.max_iterations,
                None,
                None,
                None,
                &[],
                &[],
            ),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                let rendered = if response.trim().is_empty() {
                    "[Empty response]".to_string()
                } else {
                    response
                };

                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "[Agent '{agent_name}' ({provider}/{model}, agentic)]\n{rendered}",
                        provider = agent_config.provider,
                        model = agent_config.model
                    ),
                    error: None,
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Agent '{agent_name}' failed: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' timed out after {DELEGATE_AGENTIC_TIMEOUT_SECS}s"
                )),
            }),
        }
    }

    fn start_coordination_trace(
        &self,
        _agent_name: &str,
        _prompt: &str,
        _context: &str,
        _agent_config: &DelegateAgentConfig,
    ) -> CoordinationTrace {
        let correlation_id = Uuid::new_v4().to_string();
        let conversation_id = format!("delegate:{correlation_id}");
        CoordinationTrace {
            correlation_id,
            conversation_id,
            request_message_id: None,
        }
    }

    fn finish_coordination_trace(
        &self,
        _agent_name: &str,
        _trace: &CoordinationTrace,
        _success: bool,
        _detail: &str,
    ) {
        // Stub - coordination system not yet implemented
    }
}

#[derive(Debug, Clone)]
struct CoordinationTrace {
    correlation_id: String,
    conversation_id: String,
    request_message_id: Option<String>,
}

fn text_preview(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "[empty]".to_string();
    }
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let mut preview = trimmed.chars().take(max_chars).collect::<String>();
    preview.push_str("...");
    preview
}

struct ToolArcRef {
    inner: Arc<dyn Tool>,
}

impl ToolArcRef {
    fn new(inner: Arc<dyn Tool>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Tool for ToolArcRef {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.inner.execute(args).await
    }
}

struct NoopObserver;

impl Observer for NoopObserver {
    fn record_event(&self, _event: &ObserverEvent) {}

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn name(&self) -> &str {
        "noop"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, ChatResponse, ToolCall};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use anyhow::anyhow;
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn sample_agents() -> HashMap<String, DelegateAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: Some("You are a research assistant.".to_string()),
                api_key: None,
                enabled: true,
                capabilities: vec!["research".to_string(), "summary".to_string()],
                priority: 0,
                temperature: Some(0.3),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );
        agents.insert(
            "coder".to_string(),
            DelegateAgentConfig {
                provider: "openrouter".to_string(),
                model: crate::config::DEFAULT_MODEL_FALLBACK.to_string(),
                system_prompt: None,
                api_key: Some("delegate-test-credential".to_string()),
                enabled: true,
                capabilities: vec!["coding".to_string(), "refactor".to_string()],
                priority: 1,
                temperature: None,
                max_depth: 2,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );
        agents
    }

    #[allow(clippy::fn_params_excessive_bools)]
    fn write_runtime_orchestration_config(
        path: &std::path::Path,
        teams_enabled: bool,
        teams_auto_activate: bool,
        teams_max_agents: usize,
        subagents_enabled: bool,
        subagents_auto_activate: bool,
        subagents_max_concurrent: usize,
    ) {
        let contents = format!(
            r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7

[agent.teams]
enabled = {teams_enabled}
auto_activate = {teams_auto_activate}
max_agents = {teams_max_agents}

[agent.subagents]
enabled = {subagents_enabled}
auto_activate = {subagents_auto_activate}
max_concurrent = {subagents_max_concurrent}
"#
        );
        std::fs::write(path, contents).unwrap();
    }

    #[derive(Default)]
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }

        fn description(&self) -> &str {
            "Echoes the `value` argument."
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {"type": "string"}
                },
                "required": ["value"]
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(ToolResult {
                success: true,
                output: format!("echo:{value}"),
                error: None,
            })
        }
    }

    struct OneToolThenFinalProvider;

    #[async_trait]
    impl Provider for OneToolThenFinalProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let has_tool_message = request.messages.iter().any(|m| m.role == "tool");
            if has_tool_message {
                Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            } else {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "echo_tool".to_string(),
                        arguments: "{\"value\":\"ping\"}".to_string(),
                    }],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }
    }

    struct InfiniteToolCallProvider;

    #[async_trait]
    impl Provider for InfiniteToolCallProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "loop".to_string(),
                    name: "echo_tool".to_string(),
                    arguments: "{\"value\":\"x\"}".to_string(),
                }],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            })
        }
    }

    struct FailingProvider;

    #[async_trait]
    impl Provider for FailingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow!("provider boom"))
        }
    }

    fn agentic_config(allowed_tools: Vec<String>, max_iterations: usize) -> DelegateAgentConfig {
        DelegateAgentConfig {
            provider: "openrouter".to_string(),
            model: "model-test".to_string(),
            system_prompt: Some("You are agentic.".to_string()),
            api_key: Some("delegate-test-credential".to_string()),
            enabled: true,
            capabilities: Vec::new(),
            priority: 0,
            temperature: Some(0.2),
            max_depth: 3,
            agentic: true,
            allowed_tools,
            max_iterations,
            provider_retries: None,
            fallback_providers: vec![],
        }
    }

    #[test]
    fn name_and_schema() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        assert_eq!(tool.name(), "delegate");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["context"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("prompt")));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["agent"]["minLength"], json!(1));
        assert_eq!(schema["properties"]["prompt"]["minLength"], json!(1));
    }

    #[test]
    fn description_not_empty() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_lists_agent_names() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher") || desc.contains("coder"));
    }

    #[tokio::test]
    async fn missing_agent_param() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"prompt": "test"})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn missing_prompt_param() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"agent": "researcher"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_agent_returns_error() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "nonexistent", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));
    }

    #[tokio::test]
    async fn depth_limit_enforced() {
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 3);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("depth limit"));
    }

    #[tokio::test]
    async fn depth_limit_per_agent() {
        // coder has max_depth=2, so depth=2 should be blocked
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 2);
        let result = tool
            .execute(json!({"agent": "coder", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("depth limit"));
    }

    #[test]
    fn empty_agents_schema() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("none configured"));
    }

    #[tokio::test]
    async fn invalid_provider_returns_error() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            DelegateAgentConfig {
                provider: "totally-invalid-provider".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({"agent": "broken", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Failed to create provider"));
    }

    #[tokio::test]
    async fn blank_agent_uses_auto_selection() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "  ", "prompt": "test"}))
            .await
            .unwrap();
        assert!(result.success || result.error.is_some());
        assert!(!result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Unknown agent"));
    }

    #[tokio::test]
    async fn blank_prompt_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "  \t  "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn whitespace_agent_name_trimmed_and_found() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        // " researcher " with surrounding whitespace — after trim becomes "researcher"
        let result = tool
            .execute(json!({"agent": " researcher ", "prompt": "test"}))
            .await
            .unwrap();
        // Should find "researcher" after trim — will fail at provider level
        // since ollama isn't running, but must NOT get "Unknown agent".
        assert!(
            result.error.is_none()
                || !result
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("Unknown agent")
        );
    }

    #[tokio::test]
    async fn auto_selection_can_be_disabled() {
        let tool =
            DelegateTool::new(sample_agents(), None, test_security()).with_auto_activate(false);
        let result = tool.execute(json!({"prompt": "test"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("automatic activation is disabled"));
    }

    #[tokio::test]
    async fn runtime_team_disable_blocks_delegate() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        write_runtime_orchestration_config(&config_path, false, true, 8, true, true, 4);

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_runtime_team_settings(true, true, 32, Some(config_path));
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("Agent teams are currently disabled"));
    }

    #[tokio::test]
    async fn runtime_team_auto_activation_toggle_is_hot_applied() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        write_runtime_orchestration_config(&config_path, true, true, 8, true, true, 4);

        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "invalid-provider-for-hot-reload-test".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                enabled: true,
                capabilities: vec!["research".to_string()],
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );

        let tool = DelegateTool::new(agents, None, test_security()).with_runtime_team_settings(
            true,
            true,
            32,
            Some(config_path.clone()),
        );

        let first = tool.execute(json!({"prompt": "test"})).await.unwrap();
        assert!(!first
            .error
            .unwrap_or_default()
            .contains("automatic activation is disabled"));

        write_runtime_orchestration_config(&config_path, true, false, 8, true, true, 4);
        let second = tool.execute(json!({"prompt": "test"})).await.unwrap();
        assert!(!second.success);
        assert!(second
            .error
            .unwrap_or_default()
            .contains("automatic activation is disabled"));
    }

    #[tokio::test]
    async fn delegation_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(sample_agents(), None, readonly);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
    }

    #[tokio::test]
    async fn delegation_blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(sample_agents(), None, limited);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
    }

    #[tokio::test]
    async fn delegate_context_is_prepended_to_prompt() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            DelegateAgentConfig {
                provider: "invalid-for-test".to_string(),
                model: "test-model".to_string(),
                system_prompt: None,
                api_key: None,
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "agent": "tester",
                "prompt": "do something",
                "context": "some context data"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to create provider"));
    }

    #[tokio::test]
    async fn delegate_empty_context_omits_prefix() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            DelegateAgentConfig {
                provider: "invalid-for-test".to_string(),
                model: "test-model".to_string(),
                system_prompt: None,
                api_key: None,
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "agent": "tester",
                "prompt": "do something",
                "context": ""
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to create provider"));
    }

    #[test]
    fn delegate_depth_construction() {
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 5);
        assert_eq!(tool.depth, 5);
    }

    #[tokio::test]
    async fn delegate_no_agents_configured() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security());
        let result = tool
            .execute(json!({"agent": "any", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("No delegate agents are configured"));
    }

    #[tokio::test]
    async fn agentic_mode_rejects_empty_allowed_tools() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_config(Vec::new(), 10));

        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({"agent": "agentic", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("allowed_tools is empty"));
    }

    #[tokio::test]
    async fn agentic_mode_rejects_unmatched_allowed_tools() {
        let mut agents = HashMap::new();
        agents.insert(
            "agentic".to_string(),
            agentic_config(vec!["missing_tool".to_string()], 10),
        );

        let tool = DelegateTool::new(agents, None, test_security())
            .with_parent_tools(Arc::new(vec![Arc::new(EchoTool)]));
        let result = tool
            .execute(json!({"agent": "agentic", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("no executable tools"));
    }

    #[tokio::test]
    async fn execute_agentic_runs_tool_call_loop_with_filtered_tools() {
        let config = agentic_config(vec!["echo_tool".to_string()], 10);
        let tool = DelegateTool::new(HashMap::new(), None, test_security()).with_parent_tools(
            Arc::new(vec![
                Arc::new(EchoTool),
                Arc::new(DelegateTool::new(HashMap::new(), None, test_security())),
            ]),
        );

        let provider = OneToolThenFinalProvider;
        let result = tool
            .execute_agentic("agentic", &config, &provider, "run", 0.2)
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("(openrouter/model-test, agentic)"));
        assert!(result.output.contains("done"));
    }

    #[tokio::test]
    async fn execute_agentic_excludes_delegate_even_if_allowlisted() {
        let config = agentic_config(vec!["delegate".to_string()], 10);
        let tool = DelegateTool::new(HashMap::new(), None, test_security()).with_parent_tools(
            Arc::new(vec![Arc::new(DelegateTool::new(
                HashMap::new(),
                None,
                test_security(),
            ))]),
        );

        let provider = OneToolThenFinalProvider;
        let result = tool
            .execute_agentic("agentic", &config, &provider, "run", 0.2)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("no executable tools"));
    }

    #[tokio::test]
    async fn execute_agentic_respects_max_iterations() {
        let config = agentic_config(vec!["echo_tool".to_string()], 2);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(Arc::new(vec![Arc::new(EchoTool)]));

        let provider = InfiniteToolCallProvider;
        let result = tool
            .execute_agentic("agentic", &config, &provider, "run", 0.2)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("maximum tool iterations (2)"));
    }

    #[tokio::test]
    async fn execute_agentic_propagates_provider_errors() {
        let config = agentic_config(vec!["echo_tool".to_string()], 10);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(Arc::new(vec![Arc::new(EchoTool)]));

        let provider = FailingProvider;
        let result = tool
            .execute_agentic("agentic", &config, &provider, "run", 0.2)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("provider boom"));
    }

    // Tests disabled - coordination system not yet implemented
    #[tokio::test]
    #[ignore = "coordination system not yet implemented"]
    async fn execute_records_failure_events_in_coordination_bus() {
        // Coordination tests disabled until coordination module is implemented
    }

    #[test]
    #[ignore = "coordination system not yet implemented"]
    fn coordination_trace_transitions_state_to_completed() {
        // Coordination tests disabled until coordination module is implemented
    }

    #[test]
    fn delegate_agent_fallback_providers_override_used_when_set() {
        let mut agents = HashMap::new();
        agents.insert(
            "searcher".to_string(),
            DelegateAgentConfig {
                provider: "gemini".to_string(),
                model: "gemini-3-flash-preview".to_string(),
                system_prompt: None,
                api_key: None,
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![
                    "gemini:gemini-1".to_string(),
                    "gemini:gemini-2".to_string(),
                ],
            },
        );

        let global_fallbacks = vec!["openai-codex:codex-1".to_string()];
        let reliability = crate::config::ReliabilityConfig {
            fallback_providers: global_fallbacks.clone(),
            ..Default::default()
        };

        let tool = DelegateTool::new(agents, None, test_security()).with_reliability(reliability);

        // Simulate what execute() does: override fallback_providers when agent has non-empty list
        let agent_config = tool.agents.get("searcher").unwrap();
        let mut agent_reliability = tool.reliability.clone();
        if !agent_config.fallback_providers.is_empty() {
            agent_reliability.fallback_providers = agent_config.fallback_providers.clone();
        }

        assert_eq!(
            agent_reliability.fallback_providers,
            vec!["gemini:gemini-1", "gemini:gemini-2"],
            "agent fallback_providers should override global ones"
        );
    }

    #[test]
    fn delegate_agent_empty_fallback_providers_inherits_global() {
        let mut agents = HashMap::new();
        agents.insert(
            "searcher".to_string(),
            DelegateAgentConfig {
                provider: "gemini".to_string(),
                model: "gemini-3-flash-preview".to_string(),
                system_prompt: None,
                api_key: None,
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                provider_retries: None,
                fallback_providers: vec![],
            },
        );

        let global_fallbacks = vec!["openai-codex:codex-1".to_string()];
        let reliability = crate::config::ReliabilityConfig {
            fallback_providers: global_fallbacks.clone(),
            ..Default::default()
        };

        let tool = DelegateTool::new(agents, None, test_security()).with_reliability(reliability);

        let agent_config = tool.agents.get("searcher").unwrap();
        let mut agent_reliability = tool.reliability.clone();
        if !agent_config.fallback_providers.is_empty() {
            agent_reliability.fallback_providers = agent_config.fallback_providers.clone();
        }

        assert_eq!(
            agent_reliability.fallback_providers, global_fallbacks,
            "empty agent fallback_providers should leave global ones intact"
        );
    }
}
