//! Prompt reply utilities for agent interactions.
//!
//! This module provides types and helper functions for handling prompt reply
//! interactions, where the agent is asked to provide structured responses
//! according to a given schema.

use serde_json::{Map as JsonMap, json};
use snafu::prelude::*;

use crate::provider::{Content, Message, ToolUse};
use crate::{PromptSchema, Result};

/// Tool name used for prompt reply interactions.
pub const PROMPT_REPLY_TOOL_NAME: &str = "combo_reply";

/// Error message when reply tool use is not found.
pub const REPLY_TOOL_MISSING_ERROR: &str = "reply tool use not found in response";

/// Response from a prompt reply interaction.
pub struct PromptReply {
    pub tool_use: ToolUse,
    pub response: String,
    pub thinking: Vec<String>,
    pub usage: Option<crate::provider::UsageStats>,
}

/// Build a user message that instructs the agent to use the reply tool.
pub fn build_reply_prompt_message(schemas: &[PromptSchema]) -> Message {
    Message::user(Content::Text(build_reply_tool_directive(schemas)))
}

/// Build a retry message when the agent fails to use the reply tool.
pub fn build_reply_retry_message(schemas: &[PromptSchema]) -> Message {
    let directive = build_reply_tool_directive(schemas);
    Message::user(Content::Text(format!(
        "The previous response did not call the required tool. {directive}"
    )))
}

/// Build a tool schema for the prompt reply tool based on given schemas.
pub fn build_reply_tool(schemas: &[PromptSchema]) -> Result<crate::provider::Tool> {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for schema in schemas {
        properties.insert(
            schema.name.clone(),
            json!({
                "type": "string",
                "description": schema.description.as_str(),
            }),
        );
        required.push(schema.name.clone());
    }
    ensure_whatever!(!properties.is_empty(), "schemas cannot be empty");
    let input_schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    });
    Ok(crate::provider::Tool {
        name: PROMPT_REPLY_TOOL_NAME.to_string(),
        description: "Return the response using the provided schema.".to_string(),
        input_schema,
    })
}

/// Build a directive string explaining how to use the reply tool.
pub fn build_reply_tool_directive(schemas: &[PromptSchema]) -> String {
    let fields = schemas
        .iter()
        .map(|schema| schema.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "You must call the tool \"{PROMPT_REPLY_TOOL_NAME}\" exactly once. \
Do not output plain text. Provide all required fields in the tool input. \
Required fields: {fields}."
    )
}

/// Check if tool choice fallback should be used based on request options.
pub fn should_use_tool_choice_fallback(request_options: &crate::RequestOptions) -> bool {
    request_options.disable_tool_choice && request_options.tool_choice_fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_reply_tool_with_single_schema() {
        let schemas = vec![PromptSchema {
            name: "field1".to_string(),
            description: "First field".to_string(),
        }];
        let tool = build_reply_tool(&schemas).unwrap();
        assert_eq!(tool.name, PROMPT_REPLY_TOOL_NAME);
    }

    #[test]
    fn test_build_reply_tool_with_multiple_schemas() {
        let schemas = vec![
            PromptSchema {
                name: "field1".to_string(),
                description: "First field".to_string(),
            },
            PromptSchema {
                name: "field2".to_string(),
                description: "Second field".to_string(),
            },
        ];
        let tool = build_reply_tool(&schemas).unwrap();
        assert_eq!(tool.name, PROMPT_REPLY_TOOL_NAME);
        // Check that the schema contains both fields
        let schema_obj = tool.input_schema.as_object().unwrap();
        assert!(schema_obj.get("properties").is_some());
    }
}
