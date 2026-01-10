//! System prompt handling and template substitution.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use super::config::SystemPromptConfig;

/// Errors that can occur when building a system prompt.
#[derive(Debug)]
pub enum PromptError {
    /// Failed to read prompt file.
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFile { path, source } => {
                write!(
                    f,
                    "failed to read prompt file {}: {}",
                    path.display(),
                    source
                )
            }
        }
    }
}

impl std::error::Error for PromptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } => Some(source),
        }
    }
}

/// Builder for constructing system prompts.
#[derive(Debug, Default)]
pub struct SystemPromptBuilder {
    /// Base prompt content.
    base: Option<String>,
    /// Custom content to append.
    custom: Option<String>,
    /// Template arguments.
    args: HashMap<String, String>,
}

#[allow(dead_code)]
impl SystemPromptBuilder {
    /// Create a new builder with empty base.
    /// For builtin prompts, use build_system_prompt_from_config with agent.toml config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base prompt content.
    pub fn base(mut self, content: impl Into<String>) -> Self {
        self.base = Some(content.into());
        self
    }

    /// Set custom content to append to the base prompt.
    pub fn custom(mut self, content: impl Into<String>) -> Self {
        self.custom = Some(content.into());
        self
    }

    /// Add template arguments for variable substitution.
    pub fn args(mut self, args: HashMap<String, String>) -> Self {
        self.args = args.to_owned();
        self
    }

    /// Add a single template argument.
    pub fn arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.insert(key.into(), value.into());
        self
    }

    /// Apply configuration from SystemPromptConfig.
    pub fn from_config(config: &SystemPromptConfig, base_path: &Path) -> Result<Self, PromptError> {
        match config {
            SystemPromptConfig::Inline { content, args } => Ok(Self::new()
                .base(content)
                .args(args.to_owned().unwrap_or_default())),
            SystemPromptConfig::File { path, args } => {
                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    base_path.join(path)
                };
                let content = std::fs::read_to_string(&full_path).map_err(|source| {
                    PromptError::ReadFile {
                        path: full_path,
                        source,
                    }
                })?;
                Ok(Self::new()
                    .base(content)
                    .args(args.to_owned().unwrap_or_default()))
            }
        }
    }

    /// Apply configuration from SystemPromptConfig (async version).
    pub async fn from_config_async(
        config: &SystemPromptConfig,
        base_path: &Path,
    ) -> Result<Self, PromptError> {
        match config {
            SystemPromptConfig::Inline { content, args } => Ok(Self::new()
                .base(content)
                .args(args.to_owned().unwrap_or_default())),
            SystemPromptConfig::File { path, args } => {
                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    base_path.join(path)
                };
                let content = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|source| PromptError::ReadFile {
                        path: full_path,
                        source,
                    })?;

                Ok(Self::new()
                    .base(content)
                    .args(args.to_owned().unwrap_or_default()))
            }
        }
    }

    /// Build the final system prompt string.
    pub fn build(self) -> String {
        let base = self.base.unwrap_or_default();
        let custom = self.custom.unwrap_or_default();

        // Combine base and custom
        let combined = if custom.trim().is_empty() {
            base
        } else if base.trim().is_empty() {
            custom
        } else {
            format!("{}\n\n{}", base, custom)
        };

        // Apply template substitution
        substitute_template(&combined, &self.args)
    }
}

/// Substitute template variables in the format `{{VAR}}`.
///
/// Variables that are not found in the args map are left unchanged.
pub fn substitute_template(template: &str, args: &HashMap<String, String>) -> String {
    let mut result = template.to_string();

    for (key, value) in args {
        let pattern = format!("{{{{{}}}}}", key);
        result = result.replace(&pattern, value);
    }

    result
}

/// Build system prompt from SystemPromptConfig.
///
/// Combines agent.toml system_prompt with AGENTS.md files from multiple layers:
/// 1. Base: agent.toml system_prompt configuration
/// 2. Append: global AGENTS.md (from config_dir)
/// 3. Append: workspace AGENTS.md (from workspace_dir)
///
/// If config is None, returns an empty string.
pub fn build_system_prompt_from_config(
    config: Option<&SystemPromptConfig>,
    config_dir: &Path,
    workspace_dir: &Path,
) -> String {
    // 1. Base: agent.toml system_prompt (if configured)
    let builder = if let Some(prompt_config) = config {
        match SystemPromptBuilder::from_config(prompt_config, workspace_dir) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("Warning: Failed to load system prompt from config: {}", err);
                SystemPromptBuilder::default()
            }
        }
    } else {
        SystemPromptBuilder::default()
    };

    // Build base prompt
    let mut current_prompt = builder.build();

    // 2. Append: global AGENTS.md
    let global_agents_md = config_dir.join("AGENTS.md");
    if global_agents_md.exists()
        && let Ok(content) = std::fs::read_to_string(&global_agents_md)
        && !content.trim().is_empty()
    {
        if !current_prompt.trim().is_empty() {
            current_prompt.push_str("\n\n");
        }
        current_prompt.push_str(&content);
    }

    // 3. Append: workspace AGENTS.md
    let workspace_agents_md = workspace_dir.join("AGENTS.md");
    if workspace_agents_md.exists()
        && let Ok(content) = std::fs::read_to_string(&workspace_agents_md)
        && !content.trim().is_empty()
    {
        if !current_prompt.trim().is_empty() {
            current_prompt.push_str("\n\n");
        }
        current_prompt.push_str(&content);
    }

    current_prompt
}

/// Build system prompt from SystemPromptConfig (async version).
///
/// Combines agent.toml system_prompt with AGENTS.md files from multiple layers:
/// 1. Base: agent.toml system_prompt configuration
/// 2. Append: global AGENTS.md (from config_dir)
/// 3. Append: workspace AGENTS.md (from workspace_dir)
///
/// If config is None, returns an empty string.
///
/// Note: This function is exported for use by TUI components that require async I/O.
pub async fn build_system_prompt_from_config_async(
    config: Option<&SystemPromptConfig>,
    config_dir: &Path,
    workspace_dir: &Path,
) -> String {
    // 1. Base: agent.toml system_prompt (if configured)
    let builder = if let Some(prompt_config) = config {
        match SystemPromptBuilder::from_config_async(prompt_config, workspace_dir).await {
            Ok(b) => b,
            Err(err) => {
                eprintln!("Warning: Failed to load system prompt from config: {}", err);
                SystemPromptBuilder::default()
            }
        }
    } else {
        SystemPromptBuilder::default()
    };

    // Build base prompt
    let mut current_prompt = builder.build();

    // 2. Append: global AGENTS.md
    let global_agents_md = config_dir.join("AGENTS.md");
    if global_agents_md.exists()
        && let Ok(content) = tokio::fs::read_to_string(&global_agents_md).await
        && !content.trim().is_empty()
    {
        if !current_prompt.trim().is_empty() {
            current_prompt.push_str("\n\n");
        }
        current_prompt.push_str(&content);
    }

    // 3. Append: workspace AGENTS.md
    let workspace_agents_md = workspace_dir.join("AGENTS.md");
    if workspace_agents_md.exists()
        && let Ok(content) = tokio::fs::read_to_string(&workspace_agents_md).await
        && !content.trim().is_empty()
    {
        if !current_prompt.trim().is_empty() {
            current_prompt.push_str("\n\n");
        }
        current_prompt.push_str(&content);
    }

    current_prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_single_variable() {
        let mut args = HashMap::new();
        args.insert("NAME".to_string(), "Alice".to_string());

        let result = substitute_template("Hello, {{NAME}}!", &args);
        assert_eq!(result, "Hello, Alice!");
    }

    #[test]
    fn substitute_multiple_variables() {
        let mut args = HashMap::new();
        args.insert("ROLE".to_string(), "developer".to_string());
        args.insert("PROJECT".to_string(), "code-combo".to_string());

        let result = substitute_template("You are a {{ROLE}} working on {{PROJECT}}.", &args);
        assert_eq!(result, "You are a developer working on code-combo.");
    }

    #[test]
    fn substitute_missing_variable_unchanged() {
        let args = HashMap::new();

        let result = substitute_template("Hello, {{NAME}}!", &args);
        assert_eq!(result, "Hello, {{NAME}}!");
    }

    #[test]
    fn substitute_repeated_variable() {
        let mut args = HashMap::new();
        args.insert("X".to_string(), "test".to_string());

        let result = substitute_template("{{X}} and {{X}} again", &args);
        assert_eq!(result, "test and test again");
    }

    #[test]
    fn builder_with_empty_base() {
        let prompt = SystemPromptBuilder::new().build();
        assert_eq!(prompt, "");
    }

    #[test]
    fn builder_with_custom_content() {
        let prompt = SystemPromptBuilder::new()
            .custom("Additional instructions here.")
            .build();
        assert_eq!(prompt, "Additional instructions here.");
    }

    #[test]
    fn builder_with_template_args() {
        let prompt = SystemPromptBuilder::new()
            .custom("Working on {{PROJECT}}.")
            .arg("PROJECT", "test-project")
            .build();
        assert_eq!(prompt, "Working on test-project.");
    }

    #[test]
    fn test_build_system_prompt_from_config_inline() {
        let config = SystemPromptConfig::Inline {
            content: "You are a helpful assistant. Role: {{ROLE}}".to_string(),
            args: Some(HashMap::from([(
                "ROLE".to_string(),
                "developer".to_string(),
            )])),
        };

        let prompt =
            build_system_prompt_from_config(Some(&config), Path::new("/tmp"), Path::new("/tmp"));

        assert!(prompt.contains("You are a helpful assistant"));
        assert!(prompt.contains("Role: developer"));
    }

    #[test]
    fn test_build_system_prompt_from_config_none_returns_empty() {
        // When config is None (shouldn't happen with builtin agent.toml),
        // the function returns an empty string from the builder
        let prompt = build_system_prompt_from_config(None, Path::new("/tmp"), Path::new("/tmp"));

        assert_eq!(prompt, "");
    }
}
