use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAI,
    Anthropic,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub api_key: String,
    pub base_url: String,
}
