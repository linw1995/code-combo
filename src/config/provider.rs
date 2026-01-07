use serde::{Deserialize, Serialize};

use crate::config::EnvString;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
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
