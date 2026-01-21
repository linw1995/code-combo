use serde::{Deserialize, Serialize};

use crate::config::EnvString;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[serde(rename = "openai")]
    OpenAI,
    Anthropic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingBlocksMode {
    #[default]
    DropAfterTurn,
    Keep,
    DropAlways,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelRequestConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_tool_choice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice_fallback: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_blocks: Option<ThinkingBlocksMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensure_toolcall_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stringify_nested_tool_inputs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload_combo_reply: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combo_reply_retries: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_reason: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    pub disable_tools: bool,
    pub disable_tool_choice: bool,
    pub tool_choice_fallback: bool,
    pub thinking_blocks: ThinkingBlocksMode,
    pub ensure_toolcall_thinking: bool,
    pub disable_stream: bool,
    pub stringify_nested_tool_inputs: bool,
    pub offload_combo_reply: Option<bool>,
    pub combo_reply_retries: usize,
    pub context_window: Option<usize>,
    pub can_reason: Option<bool>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
}

impl RequestOptions {
    pub(crate) fn apply_override(&mut self, override_config: &ModelRequestConfig) {
        if let Some(value) = override_config.disable_tools {
            self.disable_tools = value;
        }
        if let Some(value) = override_config.disable_tool_choice {
            self.disable_tool_choice = value;
        }
        if let Some(value) = override_config.tool_choice_fallback {
            self.tool_choice_fallback = value;
        }
        if let Some(value) = override_config.thinking_blocks {
            self.thinking_blocks = value;
        }
        if let Some(value) = override_config.ensure_toolcall_thinking {
            self.ensure_toolcall_thinking = value;
        }
        if let Some(value) = override_config.disable_stream {
            self.disable_stream = value;
        }
        if let Some(value) = override_config.stringify_nested_tool_inputs {
            self.stringify_nested_tool_inputs = value;
        }
        if let Some(value) = override_config.offload_combo_reply {
            self.offload_combo_reply = Some(value);
        }
        if let Some(value) = override_config.combo_reply_retries {
            self.combo_reply_retries = value;
        }
        if let Some(value) = override_config.context_window {
            self.context_window = Some(value);
        }
        if let Some(value) = override_config.can_reason {
            self.can_reason = Some(value);
        }
        if override_config.temperature.is_some() {
            self.temperature = override_config.temperature;
        }
        if override_config.max_tokens.is_some() {
            self.max_tokens = override_config.max_tokens;
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub api_key: EnvString,
    pub base_url: String,
    #[serde(default)]
    pub thinking_budget_tokens: Option<usize>,

    /// Optional list of supported models.
    /// If None or empty, this provider accepts any model (wildcard).
    #[serde(default)]
    pub models: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stringify_nested_tool_inputs: Option<bool>,

    /// When true, combo reply uses Bash tool to call `coco reply` command
    /// instead of the built-in combo_reply tool. This offloads the structured
    /// response extraction to an external command.
    #[serde(default)]
    pub offload_combo_reply: bool,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}
