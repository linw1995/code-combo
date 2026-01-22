//! Agent configuration types and loading logic.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Agent configuration loaded from `agent.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AgentConfig {
    /// Agent name.
    #[serde(default)]
    pub name: Option<String>,

    /// Agent description.
    #[serde(default)]
    pub description: Option<String>,

    /// Default model to use for this agent.
    #[serde(default, alias = "model")]
    pub default_model: Option<String>,

    /// System prompt configuration.
    #[serde(default)]
    pub system_prompt: Option<SystemPromptConfig>,

    /// Available tools (simple string list).
    #[serde(default)]
    pub tools: Option<Vec<String>>,

    /// Subagent configurations.
    #[serde(default)]
    pub subagents: Option<Vec<SubagentConfig>>,

    /// Safe commands configuration for bash tool.
    #[serde(default)]
    pub safe_commands: Option<SafeCommandsConfig>,
}

impl AgentConfig {
    /// Parse agent configuration from a TOML file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, AgentConfigError> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(|source| {
            AgentConfigError::ReadFile {
                path: path.as_ref().to_path_buf(),
                source,
            }
        })?;
        Self::from_toml(&content)
    }

    /// Try to parse agent configuration from a TOML file.
    /// Returns `None` if the file does not exist.
    pub fn try_from_file(path: impl AsRef<Path>) -> Result<Option<Self>, AgentConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }
        Self::from_file(path).map(Some)
    }

    /// Parse agent configuration from TOML string.
    pub fn from_toml(content: &str) -> Result<Self, AgentConfigError> {
        let wrapper: AgentConfigWrapper =
            toml::from_str(content).map_err(|source| AgentConfigError::Parse { source })?;
        Ok(wrapper.agent)
    }

    /// Merge another configuration into this one.
    /// Values from `other` take precedence when present.
    pub fn merge(&mut self, other: AgentConfig) {
        if other.name.is_some() {
            self.name = other.name;
        }
        if other.description.is_some() {
            self.description = other.description;
        }
        if other.default_model.is_some() {
            self.default_model = other.default_model;
        }
        if other.system_prompt.is_some() {
            self.system_prompt = other.system_prompt;
        }
        if other.tools.is_some() {
            self.tools = other.tools;
        }
        if other.subagents.is_some() {
            self.subagents = other.subagents;
        }
        if other.safe_commands.is_some() {
            self.safe_commands = other.safe_commands;
        }
    }
}

/// Agent configuration file name.
pub const AGENT_CONFIG_FILENAME: &str = "agent.toml";

/// Load agent configuration with the standard priority order:
/// 1. Global config: `~/.config/coco/agent.toml`
/// 2. Workspace config: `.coco/agent.toml`
///
/// Configurations are merged in order, with later configs taking precedence.
pub fn load_agent_config(
    config_dir: &Path,
    workspace_dir: &Path,
) -> Result<AgentConfig, AgentConfigError> {
    let mut config = AgentConfig::default();

    // Global config
    let global_path = config_dir.join(AGENT_CONFIG_FILENAME);
    if let Some(global) = AgentConfig::try_from_file(&global_path)? {
        config.merge(global);
    }

    // Workspace config
    let workspace_path = workspace_dir.join(".coco").join(AGENT_CONFIG_FILENAME);
    if let Some(workspace) = AgentConfig::try_from_file(&workspace_path)? {
        config.merge(workspace);
    }

    Ok(config)
}

/// Load agent configuration for a specific combo.
/// This includes the standard config plus combo-specific config.
pub fn load_agent_config_for_combo(
    config_dir: &Path,
    workspace_dir: &Path,
    combo_name: &str,
) -> Result<AgentConfig, AgentConfigError> {
    let mut config = load_agent_config(config_dir, workspace_dir)?;

    // Combo-specific config
    let combo_path = config_dir
        .join("combos")
        .join(combo_name)
        .join(AGENT_CONFIG_FILENAME);
    if let Some(combo_config) = AgentConfig::try_from_file(&combo_path)? {
        config.merge(combo_config);
    }

    Ok(config)
}

/// Load builtin agent configuration embedded in binary.
fn load_builtin_agent_config() -> Result<AgentConfig, AgentConfigError> {
    const BUILTIN_AGENT_TOML: &str = include_str!("agent.toml");
    AgentConfig::from_toml(BUILTIN_AGENT_TOML)
}

/// Load agent config with layering: builtin -> global -> workspace (always override).
pub fn load_agent_config_with_layers(
    path_layers: &crate::config::AgentPathLayers,
    config_dir: &Path,
    workspace_dir: &Path,
) -> Result<AgentConfig, AgentConfigError> {
    // Start with builtin
    let mut config = load_builtin_agent_config()?;

    // Apply global layer if exists (merges with builtin)
    if let Some(global_path_cfg) = &path_layers.global
        && let Some(path) = global_path_cfg.agent_config_path.as_deref()
    {
        let resolved = resolve_agent_config_path(path, config_dir);
        if let Some(global_config) = AgentConfig::try_from_file(&resolved)? {
            config.merge(global_config); // Merge fields
        }
    }

    // Apply workspace layer if exists (merges with global)
    if let Some(workspace_path_cfg) = &path_layers.workspace
        && let Some(path) = workspace_path_cfg.agent_config_path.as_deref()
    {
        let resolved = resolve_agent_config_path(path, workspace_dir);
        if let Some(workspace_config) = AgentConfig::try_from_file(&resolved)? {
            config.merge(workspace_config); // Merge fields
        }
    }

    Ok(config)
}

fn resolve_agent_config_path(path: &str, config_dir: &Path) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

/// Wrapper for TOML parsing to support `[agent]` section.
#[derive(Debug, Clone, Deserialize)]
struct AgentConfigWrapper {
    agent: AgentConfig,
}

/// System prompt configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SystemPromptConfig {
    /// Inline content.
    Inline {
        /// The system prompt content.
        content: String,
        /// Template arguments for variable substitution.
        #[serde(default)]
        args: Option<HashMap<String, String>>,
    },
    /// Load from external file.
    File {
        /// Path to the system prompt file.
        path: PathBuf,
        /// Template arguments for variable substitution.
        #[serde(default)]
        args: Option<HashMap<String, String>>,
    },
}

/// Tools configuration for the agent.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ToolsConfig {
    /// List of allowed tools.
    #[serde(default)]
    pub allow: Option<Vec<String>>,

    /// List of denied tools.
    #[serde(default)]
    pub deny: Option<Vec<String>>,
}

/// Model configuration for subagents.
///
/// Supports two modes:
/// - `inherit`: Inherit the model from the parent agent
/// - Custom model name: Use a specific model (e.g., "claude-3-opus")
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubagentModelConfig {
    /// Inherit model configuration from parent agent.
    #[serde(rename = "inherit")]
    Inherit,
    /// Use a custom model.
    Custom(String),
}

/// Subagent configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubagentConfig {
    /// Subagent name.
    pub name: String,

    /// Path to subagent configuration file (relative to working directory).
    /// If not provided, uses inline system_prompt.
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// Subagent description.
    #[serde(default)]
    pub description: Option<String>,

    /// Inline system prompt for the subagent.
    /// Used when path is not provided.
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Tools available to this subagent.
    #[serde(default)]
    pub tools: Option<Vec<String>>,

    /// Model configuration for this subagent.
    /// - `inherit`: Inherit model from parent agent (default behavior if not specified)
    /// - Custom model name: Use a specific model
    #[serde(default)]
    pub model: Option<SubagentModelConfig>,
}

/// Safe commands configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SafeCommandsConfig {
    /// Inline safe commands definition.
    Inline {
        /// Mode for applying these commands.
        #[serde(default)]
        mode: crate::config::SafeCommandsMode,
        /// List of safe command entries.
        commands: Vec<crate::config::SafeCommandEntry>,
    },
    /// Load safe commands from external file.
    File {
        /// Mode for applying these commands.
        #[serde(default)]
        mode: crate::config::SafeCommandsMode,
        /// Path to the safe commands file.
        path: PathBuf,
    },
}

/// Errors that can occur when loading agent configuration.
#[derive(Debug)]
pub enum AgentConfigError {
    /// Failed to read configuration file.
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Failed to parse TOML.
    Parse { source: toml::de::Error },
}

impl std::fmt::Display for AgentConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFile { path, source } => {
                write!(
                    f,
                    "failed to read agent config from {}: {}",
                    path.display(),
                    source
                )
            }
            Self::Parse { source } => {
                write!(f, "failed to parse agent config: {}", source)
            }
        }
    }
}

impl std::error::Error for AgentConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } => Some(source),
            Self::Parse { source } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[agent]
name = "test-agent"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        assert_eq!(config.name, Some("test-agent".to_string()));
        assert!(config.system_prompt.is_none());
        assert!(config.tools.is_none());
    }

    #[test]
    fn parse_inline_system_prompt() {
        let toml = r#"
[agent]
name = "test-agent"

[agent.system_prompt]
content = "You are a helpful assistant."
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        match config.system_prompt {
            Some(SystemPromptConfig::Inline { content, args }) => {
                assert_eq!(content, "You are a helpful assistant.");
                assert!(args.is_none());
            }
            _ => panic!("expected inline system prompt"),
        }
    }

    #[test]
    fn parse_file_system_prompt() {
        let toml = r#"
[agent]
name = "test-agent"

[agent.system_prompt]
path = "./prompts/system.md"

[agent.system_prompt.args]
ROLE = "developer"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        match config.system_prompt {
            Some(SystemPromptConfig::File { path, args }) => {
                assert_eq!(path, PathBuf::from("./prompts/system.md"));
                let args = args.expect("args should be present");
                assert_eq!(args.get("ROLE"), Some(&"developer".to_string()));
            }
            _ => panic!("expected file system prompt"),
        }
    }

    #[test]
    fn parse_tools_config() {
        let toml = r#"
[agent]
name = "test-agent"
tools = ["bash", "read", "write"]
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        let tools = config.tools.expect("tools should be present");
        assert_eq!(
            tools,
            vec!["bash".to_string(), "read".to_string(), "write".to_string()]
        );
    }

    #[test]
    fn parse_subagents() {
        let toml = r#"
[agent]
name = "main-agent"

[[agent.subagents]]
name = "coder"
path = "./agents/coder.toml"
description = "General software engineering tasks"

[[agent.subagents]]
name = "reviewer"
path = "./agents/reviewer.toml"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        let subagents = config.subagents.expect("subagents should be present");
        assert_eq!(subagents.len(), 2);
        assert_eq!(subagents[0].name, "coder");
        assert_eq!(
            subagents[0].path,
            Some(PathBuf::from("./agents/coder.toml"))
        );
        assert_eq!(
            subagents[0].description,
            Some("General software engineering tasks".to_string())
        );
        assert_eq!(subagents[1].name, "reviewer");
        assert!(subagents[1].description.is_none());
    }

    #[test]
    fn parse_subagent_model_inherit() {
        let toml = r#"
[agent]
name = "main-agent"

[[agent.subagents]]
name = "coder"
model = "inherit"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        let subagents = config.subagents.expect("subagents should be present");
        assert_eq!(subagents[0].model, Some(SubagentModelConfig::Inherit));
    }

    #[test]
    fn parse_subagent_model_custom() {
        let toml = r#"
[agent]
name = "main-agent"

[[agent.subagents]]
name = "coder"
model = "claude-3-opus"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        let subagents = config.subagents.expect("subagents should be present");
        assert_eq!(
            subagents[0].model,
            Some(SubagentModelConfig::Custom("claude-3-opus".to_string()))
        );
    }

    #[test]
    fn parse_subagent_model_none() {
        let toml = r#"
[agent]
name = "main-agent"

[[agent.subagents]]
name = "coder"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        let subagents = config.subagents.expect("subagents should be present");
        assert!(subagents[0].model.is_none());
    }

    #[test]
    fn merge_configs() {
        let mut base = AgentConfig {
            name: Some("base".to_string()),
            default_model: Some("claude-3".to_string()),
            ..Default::default()
        };

        let override_config = AgentConfig {
            name: Some("override".to_string()),
            description: Some("overridden".to_string()),
            ..Default::default()
        };

        base.merge(override_config);

        assert_eq!(base.name, Some("override".to_string()));
        assert_eq!(base.description, Some("overridden".to_string()));
        assert_eq!(base.default_model, Some("claude-3".to_string())); // preserved from base
    }

    #[test]
    fn load_agent_config_from_empty_dirs() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let config = load_agent_config(temp_dir.path(), temp_dir.path()).expect("load config");
        assert!(config.name.is_none());
        assert!(config.system_prompt.is_none());
    }

    #[test]
    fn load_agent_config_with_global() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let config_content = r#"
[agent]
name = "global-agent"
"#;
        std::fs::write(temp_dir.path().join("agent.toml"), config_content).expect("write config");

        let config = load_agent_config(temp_dir.path(), temp_dir.path()).expect("load config");
        assert_eq!(config.name, Some("global-agent".to_string()));
    }

    #[test]
    fn load_agent_config_workspace_overrides_global() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let workspace_dir = temp_dir.path().join("workspace");
        let coco_dir = workspace_dir.join(".coco");
        std::fs::create_dir_all(&coco_dir).expect("create coco dir");

        // Global config
        let global_content = r#"
[agent]
name = "global-agent"
model = "claude-3"
"#;
        std::fs::write(temp_dir.path().join("agent.toml"), global_content).expect("write global");

        // Workspace config
        let workspace_content = r#"
[agent]
name = "workspace-agent"
"#;
        std::fs::write(coco_dir.join("agent.toml"), workspace_content).expect("write workspace");

        let config = load_agent_config(temp_dir.path(), &workspace_dir).expect("load config");
        assert_eq!(config.name, Some("workspace-agent".to_string()));
        assert_eq!(config.default_model, Some("claude-3".to_string())); // preserved from global
    }

    #[test]
    fn test_load_builtin_agent_config() {
        let config = load_builtin_agent_config().expect("load builtin config");
        // Builtin config should have tools defined
        assert!(config.tools.is_some());
        let tools = config.tools.unwrap();
        assert!(!tools.is_empty());
        // Verify some expected tools are in the list
        assert!(tools.contains(&"bash".to_string()));
        assert!(tools.contains(&"read".to_string()));
    }

    #[test]
    fn test_load_agent_config_with_layers_builtin_only() {
        use crate::config::AgentPathLayers;

        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let layers = AgentPathLayers::default();

        let config = load_agent_config_with_layers(&layers, temp_dir.path(), temp_dir.path())
            .expect("load config");

        // Should load builtin when no custom configs
        assert!(config.tools.is_some());
    }

    #[test]
    fn test_load_agent_config_with_layers_override() {
        use crate::config::{AgentPathConfig, AgentPathLayers};

        let temp_dir = tempfile::tempdir().expect("create temp dir");

        // Create a custom agent.toml
        let custom_content = r#"
[agent]
name = "custom-agent"
tools = ["custom_tool"]
"#;
        std::fs::write(temp_dir.path().join("custom_agent.toml"), custom_content)
            .expect("write custom agent");

        // Setup layers to point to custom config
        let layers = AgentPathLayers {
            global: Some(AgentPathConfig {
                agent_config_path: Some("custom_agent.toml".to_string()),
                ..Default::default()
            }),
            workspace: None,
        };

        let config = load_agent_config_with_layers(&layers, temp_dir.path(), temp_dir.path())
            .expect("load config");

        // Custom config should override builtin
        assert_eq!(config.name, Some("custom-agent".to_string()));
        assert_eq!(config.tools, Some(vec!["custom_tool".to_string()]));
    }

    #[test]
    fn test_builtin_agent_config_has_system_prompt() {
        let config = load_builtin_agent_config().expect("load builtin");
        assert!(config.system_prompt.is_some());

        // Verify it's Inline content
        match config.system_prompt.unwrap() {
            SystemPromptConfig::Inline { content, .. } => {
                assert!(content.contains("You are Coco"));
            }
            _ => panic!("Expected Inline system prompt"),
        }
    }

    #[test]
    fn parse_inline_safe_commands() {
        let toml = r#"
[agent]
name = "test-agent"

[agent.safe_commands]
mode = "override"

[[agent.safe_commands.commands]]
name = "cat"
allow_any = true
allow_positional = true
positional_path_from = 0

[[agent.safe_commands.commands]]
command = ["git", "status"]
allow_any = true
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        match config.safe_commands {
            Some(SafeCommandsConfig::Inline { mode, commands }) => {
                assert_eq!(mode, crate::config::SafeCommandsMode::Override);
                assert_eq!(commands.len(), 2);
                assert_eq!(commands[0].name, Some("cat".to_string()));
                assert!(commands[0].allow_any);
                assert_eq!(commands[0].positional_path_from, Some(0));
                assert_eq!(
                    commands[1].command,
                    Some(vec!["git".to_string(), "status".to_string()])
                );
            }
            _ => panic!("expected inline safe commands"),
        }
    }

    #[test]
    fn parse_file_safe_commands() {
        let toml = r#"
[agent]
name = "test-agent"

[agent.safe_commands]
mode = "append"
path = "./custom_safe_commands.toml"
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        match config.safe_commands {
            Some(SafeCommandsConfig::File { mode, path }) => {
                assert_eq!(mode, crate::config::SafeCommandsMode::Append);
                assert_eq!(path, PathBuf::from("./custom_safe_commands.toml"));
            }
            _ => panic!("expected file safe commands"),
        }
    }

    #[test]
    fn parse_safe_commands_with_detailed_flags() {
        let toml = r#"
[agent]
name = "test-agent"

[agent.safe_commands]

[[agent.safe_commands.commands]]
name = "ls"
allow_any = true
flags = [
  { name = "-l", arg = "none" },
  { name = "--color", arg = "optional" },
]
"#;
        let config = AgentConfig::from_toml(toml).expect("parse config");
        match config.safe_commands {
            Some(SafeCommandsConfig::Inline { mode, commands }) => {
                assert_eq!(mode, crate::config::SafeCommandsMode::Append); // default
                assert_eq!(commands.len(), 1);
                let ls_cmd = &commands[0];
                assert_eq!(ls_cmd.name, Some("ls".to_string()));
                assert_eq!(ls_cmd.flags.len(), 2);
                assert_eq!(ls_cmd.flags[0].name, "-l");
                assert_eq!(ls_cmd.flags[0].arg, crate::config::FlagValuePolicy::None);
                assert_eq!(ls_cmd.flags[1].name, "--color");
                assert_eq!(
                    ls_cmd.flags[1].arg,
                    crate::config::FlagValuePolicy::Optional
                );
            }
            _ => panic!("expected inline safe commands"),
        }
    }

    #[test]
    fn merge_safe_commands_config() {
        let mut base = AgentConfig {
            name: Some("base".to_string()),
            safe_commands: Some(SafeCommandsConfig::Inline {
                mode: crate::config::SafeCommandsMode::Append,
                commands: vec![crate::config::SafeCommandEntry {
                    name: Some("cat".to_string()),
                    allow_any: true,
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let override_config = AgentConfig {
            safe_commands: Some(SafeCommandsConfig::File {
                mode: crate::config::SafeCommandsMode::Override,
                path: PathBuf::from("./override.toml"),
            }),
            ..Default::default()
        };

        base.merge(override_config);

        match base.safe_commands {
            Some(SafeCommandsConfig::File { mode, path }) => {
                assert_eq!(mode, crate::config::SafeCommandsMode::Override);
                assert_eq!(path, PathBuf::from("./override.toml"));
            }
            _ => panic!("expected file safe commands after merge"),
        }
    }
}
