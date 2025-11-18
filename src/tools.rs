use async_trait::async_trait;
use serde_json::{Value, json};

#[async_trait]
pub trait Tool: Send + Sync {
    /// Human-readable name
    fn name(&self) -> &'static str;

    /// Description for the LLM or UI
    fn description(&self) -> &'static str;

    /// JSON schema for the input (optional for validation)
    fn input_schema(&self) -> Value {
        json!({
            "type": "object"
        })
    }

    /// Execute the tool with a JSON input, producing JSON output
    async fn execute(&self, input: Value) -> ExecuteResult;
}

#[derive(Debug, Clone)]
pub enum Output {
    Json(Value),
    Message(String),
}

/// Result for LLM
pub type ExecuteResult = Result<Output, Output>;

impl TryFrom<&Output> for anthropic::Content {
    type Error = serde_json::Error;

    fn try_from(value: &Output) -> Result<Self, Self::Error> {
        Ok(match value {
            Output::Json(value) => Self::Text(serde_json::to_string(&value)?),
            Output::Message(message) => Self::Text(message.to_owned()),
        })
    }
}

impl From<&str> for Output {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

impl From<String> for Output {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<Value> for Output {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl Output {
    fn ok(self) -> ExecuteResult {
        Ok(self)
    }

    fn err(self) -> ExecuteResult {
        Err(self)
    }
}

macro_rules! err_msg {
    ($template:literal) => {
        crate::tools::Output::from(format!($template)).err()
    };
    ($template:literal, $expression:expr) => {
        crate::tools::Output::from(format!($template, $expression)).err()
    };
    ($template:literal, $($expression:expr),* ) => {
        crate::tools::Output::from(format!($template, $($expression),*)).err()
    };
}

mod bash;
mod read;
mod str_replace;

pub use bash::{BASH_TOOL_NAME, BashInput, BashOutput, BashTool};
pub use read::{READ_TOOL_NAME, ReadInput, ReadTool};
pub use str_replace::{STR_REPLACE_TOOL_NAME, StrReplaceInput, StrReplaceTool};
