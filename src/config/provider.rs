use serde::{Deserialize, Serialize};

use crate::config::EnvString;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[serde(rename = "openai")]
    OpenAI,
    Anthropic,
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
