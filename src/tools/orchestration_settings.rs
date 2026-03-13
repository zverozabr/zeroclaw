//! Runtime orchestration settings loader.
//!
//! Reads [agent.teams] and [agent.subagents] from runtime config TOML.

use crate::config::{AgentTeamsConfig, SubAgentsConfig};
use std::path::Path;

/// Load orchestration settings from a runtime config file.
/// Returns (teams_config, subagents_config).
pub fn load_orchestration_settings(
    path: &Path,
) -> anyhow::Result<(AgentTeamsConfig, SubAgentsConfig)> {
    let contents = std::fs::read_to_string(path)?;
    let parsed: toml::Value = toml::from_str(&contents)?;

    let teams = if let Some(agent_section) = parsed.get("agent") {
        if let Some(teams_section) = agent_section.get("teams") {
            toml::from_str(&toml::to_string(teams_section)?)?
        } else {
            AgentTeamsConfig::default()
        }
    } else {
        AgentTeamsConfig::default()
    };

    let subagents = if let Some(agent_section) = parsed.get("agent") {
        if let Some(subagents_section) = agent_section.get("subagents") {
            toml::from_str(&toml::to_string(subagents_section)?)?
        } else {
            SubAgentsConfig::default()
        }
    } else {
        SubAgentsConfig::default()
    };

    Ok((teams, subagents))
}
