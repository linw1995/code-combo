use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    ToolUse(ToolUse),
    ToolResult {
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        /// Note: Content with limited block types, currently Text blocks only
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Multiple(Vec<Block>),
}

impl Content {
    pub fn user_cancel(self) -> Self {
        match self {
            Self::Text(text) => Self::Text(format!("User cancelled!\n{text}")),
            Self::Multiple(mut blocks) => {
                blocks.insert(0, "User cancelled!".into());
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn text_content_serialization() {
        let m = Message::user("Hello".into());
        let rv = serde_json::to_value(m).unwrap();
        assert_eq!(
            rv,
            json!({
                "role": "user",
                "content": "Hello"
            })
        )
    }

    #[test]
    fn multiple_content_serialization() {
        let content: &[Block] = &[
            "I'll check the current weather in San Francisco for you.".into(),
            Block::tool_use(
                "toolu_01A09q90qw90lq917835lq9",
                "get_weather",
                json!({
                    "location": "San Francisco, CA",
                    "unit": "celsius"
                }),
            ),
        ];
        let m = Message::assistant(content.into());
        let rv = serde_json::to_value(m).unwrap();
        assert_eq!(
            rv,
            json!({
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "I'll check the current weather in San Francisco for you."
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_01A09q90qw90lq917835lq9",
                        "name": "get_weather",
                        "input": {
                            "location": "San Francisco, CA",
                            "unit": "celsius"
                        }
                    }
                ]
            })
        )
    }

    #[test]
    fn tool_result_serialization() {
        let content: &[Block] = &[Block::tool_result(
            "toolu_01A09q90qw90lq917835lq9",
            None,
            "15 degrees".into(),
        )];
        let m = Message::user(content.into());
        let rv = serde_json::to_value(m).unwrap();
        assert_eq!(
            rv,
            json!(        {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_01A09q90qw90lq917835lq9",
                        "content": "15 degrees"
                    }
                ]
            })
        )
    }

    #[test]
    fn tool_serialization() {
        let tool = Tool {
            name: "get_weather".to_string(),
            description: "Get the current weather in a given location".to_string(),
            input_schema: json!({
                "properties": {
                  "location": {
                    "description": "The city and state, e.g. San Francisco, CA",
                    "type": "string"
                  },
                  "unit": {
                    "description": "Unit for the output - one of (celsius, fahrenheit)",
                    "type": "string"
                  }
                },
                "required": ["location"],
                "type": "object"
            }),
        };
        let rv = serde_json::to_value(tool).unwrap();
        assert_eq!(
            rv,
            json!({
              "description": "Get the current weather in a given location",
              "input_schema": {
                "properties": {
                  "location": {
                    "description": "The city and state, e.g. San Francisco, CA",
                    "type": "string"
                  },
                  "unit": {
                    "description": "Unit for the output - one of (celsius, fahrenheit)",
                    "type": "string"
                  }
                },
                "required": ["location"],
                "type": "object"
              },
              "name": "get_weather"
            })
        )
    }
}
