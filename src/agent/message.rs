//! Message processing utilities for agent chat interactions.
//!
//! This module provides functions for transforming and processing messages,
//! including thinking block handling and tool input stringification.

use serde_json::{Map as JsonMap, Value, json};

use super::Executor;
use crate::provider::{Block, Content, Message, Role};

/// Strip thinking blocks from a single message if it becomes empty after stripping.
pub fn strip_thinking_block(message: &Message) -> Option<Message> {
    let content = match &message.content {
        Content::Multiple(blocks) => {
            let filtered: Vec<Block> = blocks
                .iter()
                .filter(|block| !matches!(block, Block::Thinking { .. }))
                .cloned()
                .collect();
            Content::Multiple(filtered)
        }
        Content::Text(_) => message.content.clone(),
    };
    if matches!(content, Content::Multiple(ref blocks) if blocks.is_empty()) {
        None
    } else {
        Some(Message {
            role: message.role.clone(),
            content,
        })
    }
}

/// Strip all thinking blocks from a slice of messages.
pub fn strip_thinking_blocks(messages: &[Message]) -> Vec<Message> {
    messages.iter().filter_map(strip_thinking_block).collect()
}

/// Ensure that tool call messages have thinking blocks inserted.
pub fn ensure_thinking_blocks(messages: &mut [Message]) {
    for message in messages {
        if !matches!(message.role, Role::Assistant) {
            continue;
        }
        let Content::Multiple(blocks) = &mut message.content else {
            continue;
        };
        let has_tool_use = blocks
            .iter()
            .any(|block| matches!(block, Block::ToolUse(_)));
        if !has_tool_use {
            continue;
        }
        let has_thinking = blocks
            .iter()
            .any(|block| matches!(block, Block::Thinking { .. }));
        if !has_thinking {
            blocks.insert(
                0,
                Block::Thinking {
                    thinking: String::new(),
                    signature: None,
                },
            );
        }
    }
}

/// Convert nested tool schemas to string schemas for compatibility.
pub fn stringify_nested_tool_schema(schema: &Value) -> Value {
    let mut output = schema.clone();
    stringify_nested_schema_in_place(&mut output, 0);
    output
}

/// Recursively convert nested object/array schemas to string schemas.
fn stringify_nested_schema_in_place(schema: &mut Value, depth: usize) {
    if depth > 0 && schema_is_object_or_array(schema) {
        let desc = schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let suffix = "JSON string";
        let description = if desc.is_empty() {
            suffix.to_string()
        } else {
            format!("{desc} ({suffix})")
        };
        *schema = json!({
            "type": "string",
            "description": description,
        });
        return;
    }

    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for value in props.values_mut() {
            stringify_nested_schema_in_place(value, depth + 1);
        }
    }
    if let Some(items) = obj.get_mut("items") {
        stringify_nested_schema_in_place(items, depth + 1);
    }
    if let Some(additional) = obj.get_mut("additionalProperties") {
        stringify_nested_schema_in_place(additional, depth + 1);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(list) = obj.get_mut(key).and_then(Value::as_array_mut) {
            for value in list {
                stringify_nested_schema_in_place(value, depth + 1);
            }
        }
    }
}

/// Check if a JSON schema represents an object or array type.
fn schema_is_object_or_array(schema: &Value) -> bool {
    if let Some(ty) = schema.get("type") {
        match ty {
            Value::String(value) => {
                if value == "object" || value == "array" {
                    return true;
                }
            }
            Value::Array(values) => {
                if values.iter().any(|value| {
                    matches!(value, Value::String(value) if value == "object" || value == "array")
                }) {
                    return true;
                }
            }
            _ => (),
        }
    }
    if schema.get("properties").is_some() || schema.get("items").is_some() {
        return true;
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(list) = schema.get(key).and_then(Value::as_array)
            && list.iter().any(schema_is_object_or_array)
        {
            return true;
        }
    }
    false
}

/// Convert nested tool inputs to stringified JSON for compatibility.
pub fn stringify_nested_tool_inputs(messages: Vec<Message>, executor: &Executor) -> Vec<Message> {
    messages
        .into_iter()
        .map(|mut message| {
            let Content::Multiple(blocks) = &mut message.content else {
                return message;
            };
            for block in blocks {
                let Block::ToolUse(tool_use) = block else {
                    continue;
                };
                if let Some(schema) = executor.tool_input_schema(&tool_use.name) {
                    tool_use.input = stringify_tool_input_value(&tool_use.input, &schema);
                }
            }
            message
        })
        .collect()
}

/// Stringify object/array values in tool input according to schema.
fn stringify_tool_input_value(input: &Value, schema: &Value) -> Value {
    let Value::Object(map) = input else {
        return input.clone();
    };
    let mut output = JsonMap::new();
    for (key, value) in map {
        let prop_schema = schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|props| props.get(key));
        if let Some(schema) = prop_schema
            && schema_is_object_or_array(schema)
            && matches!(value, Value::Object(_) | Value::Array(_))
        {
            let text = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            output.insert(key.clone(), Value::String(text));
        } else {
            output.insert(key.clone(), value.clone());
        }
    }
    Value::Object(output)
}

/// Parse stringified JSON values back to objects/arrays.
pub fn parse_stringified_tool_input(input: Value, schema: &Value) -> Value {
    let Value::Object(map) = input else {
        return input;
    };
    let mut output = JsonMap::new();
    for (key, value) in map {
        let prop_schema = schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|props| props.get(&key));
        if let Some(schema) = prop_schema
            && schema_is_object_or_array(schema)
            && let Value::String(text) = &value
            && let Ok(parsed) = serde_json::from_str::<Value>(text)
        {
            output.insert(key, parsed);
            continue;
        }
        output.insert(key, value);
    }
    Value::Object(output)
}

/// Parse stringified tool inputs in a message back to objects/arrays.
pub fn parse_stringified_tool_inputs_in_message(message: &mut Message, executor: &Executor) {
    let Content::Multiple(blocks) = &mut message.content else {
        return;
    };
    for block in blocks {
        let Block::ToolUse(tool_use) = block else {
            continue;
        };
        if let Some(schema) = executor.tool_input_schema(&tool_use.name) {
            tool_use.input = parse_stringified_tool_input(tool_use.input.clone(), &schema);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stringify_nested_schema_converts_nested_object_and_array() {
        let schema = json!({
            "type": "object",
            "properties": {
                "meta": {
                    "type": "object",
                    "description": "metadata",
                    "properties": {
                        "name": {"type": "string"}
                    }
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            }
        });

        let output = stringify_nested_tool_schema(&schema);
        let props = output
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties");
        assert_eq!(props["meta"]["type"], "string");
        assert!(
            props["meta"]["description"]
                .as_str()
                .unwrap_or("")
                .contains("JSON string")
        );
        assert_eq!(props["tags"]["type"], "string");
    }

    #[test]
    fn stringify_and_parse_tool_input_round_trip() {
        let schema = json!({
            "type": "object",
            "properties": {
                "meta": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"}
                    }
                },
                "name": {"type": "string"}
            }
        });

        let input = json!({
            "meta": {"name": "coco"},
            "name": "demo"
        });
        let stringified = stringify_tool_input_value(&input, &schema);
        assert!(stringified.get("meta").is_some());
        assert!(stringified["meta"].is_string());

        let parsed = parse_stringified_tool_input(stringified, &schema);
        assert_eq!(parsed, input);
    }
}
