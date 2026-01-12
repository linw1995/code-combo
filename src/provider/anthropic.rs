use serde_json::Value;

use ::anthropic as anthropic_api;

use crate::provider::types::{
    Block, Content, ContentBlockDelta, Message, MessageDelta, MessagesResponse,
    MessagesStreamEvent, Role, StopReason, StreamErrorDetail, StreamUsage, Thinking, Tool,
    ToolChoice, ToolUse,
};

impl From<anthropic_api::Role> for Role {
    fn from(value: anthropic_api::Role) -> Self {
        match value {
            anthropic_api::Role::User => Self::User,
            anthropic_api::Role::Assistant => Self::Assistant,
        }
    }
}

impl From<Role> for anthropic_api::Role {
    fn from(value: Role) -> Self {
        match value {
            Role::User => Self::User,
            Role::Assistant => Self::Assistant,
        }
    }
}

impl From<anthropic_api::ToolUse> for ToolUse {
    fn from(value: anthropic_api::ToolUse) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: value.input,
        }
    }
}

impl From<ToolUse> for anthropic_api::ToolUse {
    fn from(value: ToolUse) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: value.input,
        }
    }
}

impl From<anthropic_api::Block> for Block {
    fn from(value: anthropic_api::Block) -> Self {
        match value {
            anthropic_api::Block::Text { text } => Self::Text { text },
            anthropic_api::Block::Thinking {
                thinking,
                signature,
            } => Self::Thinking {
                thinking,
                signature,
            },
            anthropic_api::Block::ToolUse(tool_use) => Self::ToolUse(tool_use.into()),
            anthropic_api::Block::ToolResult {
                tool_use_id,
                is_error,
                content,
            } => Self::ToolResult {
                tool_use_id,
                is_error,
                content: content.into(),
            },
        }
    }
}

impl From<Block> for anthropic_api::Block {
    fn from(value: Block) -> Self {
        match value {
            Block::Text { text } => Self::Text { text },
            Block::Thinking {
                thinking,
                signature,
            } => Self::Thinking {
                thinking,
                signature,
            },
            Block::ToolUse(tool_use) => Self::ToolUse(tool_use.into()),
            Block::ToolResult {
                tool_use_id,
                is_error,
                content,
            } => Self::ToolResult {
                tool_use_id,
                is_error,
                content: content.into(),
            },
        }
    }
}

impl From<anthropic_api::Content> for Content {
    fn from(value: anthropic_api::Content) -> Self {
        match value {
            anthropic_api::Content::Text(text) => Self::Text(text),
            anthropic_api::Content::Multiple(blocks) => {
                Self::Multiple(blocks.into_iter().map(Into::into).collect())
            }
        }
    }
}

impl From<Content> for anthropic_api::Content {
    fn from(value: Content) -> Self {
        match value {
            Content::Text(text) => Self::Text(text),
            Content::Multiple(blocks) => {
                Self::Multiple(blocks.into_iter().map(Into::into).collect())
            }
        }
    }
}

impl From<anthropic_api::Message> for Message {
    fn from(value: anthropic_api::Message) -> Self {
        Self {
            role: value.role.into(),
            content: value.content.into(),
        }
    }
}

impl From<Message> for anthropic_api::Message {
    fn from(value: Message) -> Self {
        Self {
            role: value.role.into(),
            content: value.content.into(),
        }
    }
}

impl From<anthropic_api::Tool> for Tool {
    fn from(value: anthropic_api::Tool) -> Self {
        Self {
            name: value.name,
            description: value.description,
            input_schema: value.input_schema,
        }
    }
}

impl From<Tool> for anthropic_api::Tool {
    fn from(value: Tool) -> Self {
        Self {
            name: value.name,
            description: value.description,
            input_schema: value.input_schema,
        }
    }
}

impl From<anthropic_api::StopReason> for StopReason {
    fn from(value: anthropic_api::StopReason) -> Self {
        match value {
            anthropic_api::StopReason::EndTurn => Self::EndTurn,
            anthropic_api::StopReason::MaxTokens => Self::MaxTokens,
            anthropic_api::StopReason::StopSequence => Self::StopSequence,
            anthropic_api::StopReason::ToolUse => Self::ToolUse,
            anthropic_api::StopReason::PauseTurn => Self::PauseTurn,
            anthropic_api::StopReason::Refusal => Self::Refusal,
        }
    }
}

impl From<StopReason> for anthropic_api::StopReason {
    fn from(value: StopReason) -> Self {
        match value {
            StopReason::EndTurn => Self::EndTurn,
            StopReason::MaxTokens => Self::MaxTokens,
            StopReason::StopSequence => Self::StopSequence,
            StopReason::ToolUse => Self::ToolUse,
            StopReason::PauseTurn => Self::PauseTurn,
            StopReason::Refusal => Self::Refusal,
        }
    }
}

impl From<anthropic_api::ToolChoice> for ToolChoice {
    fn from(value: anthropic_api::ToolChoice) -> Self {
        match value {
            anthropic_api::ToolChoice::Auto {
                disable_parallel_tool_use,
            } => Self::Auto {
                disable_parallel_tool_use,
            },
            anthropic_api::ToolChoice::Any {
                disable_parallel_tool_use,
            } => Self::Any {
                disable_parallel_tool_use,
            },
            anthropic_api::ToolChoice::Tool {
                name,
                disable_parallel_tool_use,
            } => Self::Tool {
                name,
                disable_parallel_tool_use,
            },
            anthropic_api::ToolChoice::None => Self::None,
        }
    }
}

impl From<ToolChoice> for anthropic_api::ToolChoice {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Auto {
                disable_parallel_tool_use,
            } => Self::Auto {
                disable_parallel_tool_use,
            },
            ToolChoice::Any {
                disable_parallel_tool_use,
            } => Self::Any {
                disable_parallel_tool_use,
            },
            ToolChoice::Tool {
                name,
                disable_parallel_tool_use,
            } => Self::Tool {
                name,
                disable_parallel_tool_use,
            },
            ToolChoice::None => Self::None,
        }
    }
}

impl From<anthropic_api::Thinking> for Thinking {
    fn from(value: anthropic_api::Thinking) -> Self {
        match value {
            anthropic_api::Thinking::Enabled { budget_tokens } => Self::Enabled { budget_tokens },
        }
    }
}

impl From<Thinking> for anthropic_api::Thinking {
    fn from(value: Thinking) -> Self {
        match value {
            Thinking::Enabled { budget_tokens } => Self::Enabled { budget_tokens },
        }
    }
}

impl From<anthropic_api::ContentBlockDelta> for ContentBlockDelta {
    fn from(value: anthropic_api::ContentBlockDelta) -> Self {
        match value {
            anthropic_api::ContentBlockDelta::TextDelta { text } => Self::TextDelta { text },
            anthropic_api::ContentBlockDelta::InputJsonDelta { partial_json } => {
                Self::InputJsonDelta { partial_json }
            }
            anthropic_api::ContentBlockDelta::ThinkingDelta { thinking } => {
                Self::ThinkingDelta { thinking }
            }
            anthropic_api::ContentBlockDelta::SignatureDelta { signature } => {
                Self::SignatureDelta { signature }
            }
            anthropic_api::ContentBlockDelta::Unknown => Self::Unknown,
        }
    }
}

impl From<ContentBlockDelta> for anthropic_api::ContentBlockDelta {
    fn from(value: ContentBlockDelta) -> Self {
        match value {
            ContentBlockDelta::TextDelta { text } => Self::TextDelta { text },
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                Self::InputJsonDelta { partial_json }
            }
            ContentBlockDelta::ThinkingDelta { thinking } => Self::ThinkingDelta { thinking },
            ContentBlockDelta::SignatureDelta { signature } => Self::SignatureDelta { signature },
            ContentBlockDelta::Unknown => Self::Unknown,
        }
    }
}

impl From<anthropic_api::MessageDelta> for MessageDelta {
    fn from(value: anthropic_api::MessageDelta) -> Self {
        Self {
            stop_reason: value.stop_reason.map(Into::into),
            stop_sequence: value.stop_sequence,
        }
    }
}

impl From<MessageDelta> for anthropic_api::MessageDelta {
    fn from(value: MessageDelta) -> Self {
        Self {
            stop_reason: value.stop_reason.map(Into::into),
            stop_sequence: value.stop_sequence,
        }
    }
}

impl From<anthropic_api::MessagesResponse> for MessagesResponse {
    fn from(value: anthropic_api::MessagesResponse) -> Self {
        Self {
            content: value.content.into_iter().map(Into::into).collect(),
            stop_reason: value.stop_reason.map(Into::into),
            stop_sequence: value.stop_sequence,
        }
    }
}

impl From<anthropic_api::StreamErrorDetail> for StreamErrorDetail {
    fn from(value: anthropic_api::StreamErrorDetail) -> Self {
        Self {
            message: value.message,
            r#type: value.r#type,
            code: value.code,
        }
    }
}

fn convert_stream_usage(usage: anthropic_api::StreamUsage) -> StreamUsage {
    serde_json::to_value(usage).unwrap_or(Value::Null)
}

impl From<anthropic_api::MessagesStreamEvent> for MessagesStreamEvent {
    fn from(value: anthropic_api::MessagesStreamEvent) -> Self {
        match value {
            anthropic_api::MessagesStreamEvent::MessageStart { message } => Self::MessageStart {
                message: message.into(),
            },
            anthropic_api::MessagesStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => Self::ContentBlockStart {
                index,
                content_block: content_block.into(),
            },
            anthropic_api::MessagesStreamEvent::ContentBlockDelta { index, delta } => {
                Self::ContentBlockDelta {
                    index,
                    delta: delta.into(),
                }
            }
            anthropic_api::MessagesStreamEvent::ContentBlockStop { index } => {
                Self::ContentBlockStop { index }
            }
            anthropic_api::MessagesStreamEvent::MessageDelta { delta, usage } => {
                Self::MessageDelta {
                    delta: delta.into(),
                    usage: usage.map(convert_stream_usage),
                }
            }
            anthropic_api::MessagesStreamEvent::MessageStop => Self::MessageStop,
            anthropic_api::MessagesStreamEvent::Ping => Self::Ping,
            anthropic_api::MessagesStreamEvent::Error { error } => Self::Error {
                error: error.into(),
            },
            anthropic_api::MessagesStreamEvent::Unknown { event, data } => {
                Self::Unknown { event, data }
            }
        }
    }
}
