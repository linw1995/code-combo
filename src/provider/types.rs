use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct UsageStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<usize>,
}

impl UsageStats {
    pub fn merge(&mut self, other: UsageStats) {
        let updated_input = other.input_tokens.is_some();
        let updated_output = other.output_tokens.is_some();
        let updated_total = other.total_tokens.is_some();
        if other.input_tokens.is_some() {
            self.input_tokens = other.input_tokens;
        }
        if other.output_tokens.is_some() {
            self.output_tokens = other.output_tokens;
        }
        if other.total_tokens.is_some() {
            self.total_tokens = other.total_tokens;
        }
        if !updated_total
            && (updated_input || updated_output || self.total_tokens.is_none())
            && let (Some(input), Some(output)) = (self.input_tokens, self.output_tokens)
        {
            self.total_tokens = Some(input + output);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Block {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse(ToolUse),
    ToolResult {
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        content: Content,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl Block {
    pub fn text(text: &str) -> Self {
        Self::Text {
            text: text.to_string(),
        }
    }

    pub fn tool_use(id: &str, name: &str, input: Value) -> Self {
        Self::ToolUse(ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        })
    }

    pub fn tool_result(tool_use_id: &str, is_error: Option<bool>, content: Content) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            is_error,
            content,
        }
    }
}

impl From<&str> for Block {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<String> for Block {
    fn from(value: String) -> Self {
        Self::Text { text: value }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Multiple(Vec<Block>),
}

impl Content {
    pub fn user_cancel(self) -> Self {
        let msg = Block::text("User interrupted!");
        match self {
            Self::Text(text) => Self::Multiple(vec![msg, text.into()]),
            Self::Multiple(mut blocks) => {
                blocks.insert(0, msg);
                Self::Multiple(blocks)
            }
        }
    }
}

impl From<&str> for Content {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<&[Block]> for Content {
    fn from(slice: &[Block]) -> Self {
        Self::Multiple(slice.to_vec())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Content,
}

impl Message {
    pub fn assistant(content: Content) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    pub fn user(content: Content) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    None,
}

impl ToolChoice {
    pub fn auto(disable_parallel_tool_use: Option<bool>) -> Self {
        Self::Auto {
            disable_parallel_tool_use,
        }
    }

    pub fn any(disable_parallel_tool_use: Option<bool>) -> Self {
        Self::Any {
            disable_parallel_tool_use,
        }
    }

    pub fn tool(name: &str, disable_parallel_tool_use: Option<bool>) -> Self {
        Self::Tool {
            name: name.to_string(),
            disable_parallel_tool_use,
        }
    }

    pub fn none() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Thinking {
    Enabled { budget_tokens: usize },
}

impl Thinking {
    pub fn enabled(budget_tokens: usize) -> Self {
        Self::Enabled { budget_tokens }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessagesResponse {
    pub content: Vec<Block>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ContentBlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamErrorDetail {
    pub message: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

pub type StreamUsage = UsageStats;

#[derive(Debug, Clone)]
pub enum MessagesStreamEvent {
    MessageStart {
        message: MessagesResponse,
    },
    ContentBlockStart {
        index: usize,
        content_block: Block,
    },
    ContentBlockDelta {
        index: usize,
        delta: ContentBlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: Option<StreamUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: StreamErrorDetail,
    },
    Unknown {
        event: String,
        data: Value,
    },
}

#[cfg(test)]
mod tests {
    use super::UsageStats;

    #[test]
    fn usage_merge_recomputes_total() {
        let mut usage = UsageStats {
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: None,
        };
        usage.merge(UsageStats {
            input_tokens: None,
            output_tokens: Some(5),
            total_tokens: None,
        });
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(15));
    }
}
