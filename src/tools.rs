use std::any::Any;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

macro_rules! err_msg {
    ($template:literal) => {
        crate::tools::Final::from(format!($template)).err()
    };
    ($template:literal, $expression:expr) => {
        crate::tools::Final::from(format!($template, $expression)).err()
    };
    ($template:literal, $($expression:expr),* ) => {
        crate::tools::Final::from(format!($template, $($expression),*)).err()
    };
}

mod bash;
mod list;
mod read;
mod run_combo;
mod run_task;
mod str_replace;

use crate::{AppliedTextEdit, TextEdit};
pub(crate) use bash::extra_envs_for_bash_input;
pub(crate) use bash::run_bash_chunked;
pub use bash::{BASH_TOOL_NAME, BashInput, BashOutput, BashTool, prepare_mcp_envs};
pub use list::{DEFAULT_ENTRY_LIMIT, LIST_TOOL_NAME, ListInput, ListTool, MAX_ENTRY_LIMIT};
pub use read::{
    DEFAULT_LINE_LIMIT, DEFAULT_LINE_OFFSET, MAX_LINE_LIMIT, READ_TOOL_NAME, ReadInput, ReadTool,
};
pub use run_combo::{
    ComboEvent, ComboInfo, ComboStreamKind, RUN_COMBO_TOOL_NAME, RunComboContext, RunComboInput,
    RunComboOutput, run_combo,
};
pub use run_task::{
    RUN_TASK_TOOL_NAME, RunTaskContext, RunTaskInput, RunTaskOutput, RunTaskTool, SubagentEvent,
    ToolStatus, run_task,
};
pub use str_replace::{STR_REPLACE_TOOL_NAME, StrReplaceInput, StrReplaceTool};

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
    async fn execute<'a>(&self, input: Input<'a>) -> ExecuteResult;

    /// Returns self as Any for downcasting to concrete types.
    /// Tools that need special handling in executor should override this.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }
}

#[derive(Debug)]
pub enum Input<'a> {
    Starter(Value),
    AppliedTextEdit(AppliedTextEdit<'a>),
}

#[derive(Debug)]
pub enum Output {
    TextEdit(TextEdit),
    Final(Final),
}

impl From<TextEdit> for Output {
    fn from(value: TextEdit) -> Self {
        Self::TextEdit(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Final {
    Json(Value),
    Message(String),
}

impl From<Final> for Output {
    fn from(value: Final) -> Self {
        Self::Final(value)
    }
}

/// Result for LLM
pub type ExecuteResult = Result<Output, Final>;

impl TryFrom<&Final> for crate::provider::Content {
    type Error = serde_json::Error;

    fn try_from(value: &Final) -> Result<Self, Self::Error> {
        Ok(match value {
            Final::Json(value) => Self::Text(serde_json::to_string(&value)?),
            Final::Message(message) => Self::Text(message.to_owned()),
        })
    }
}

impl From<&str> for Final {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

impl From<String> for Final {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<Value> for Final {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl Final {
    fn ok(self) -> ExecuteResult {
        Ok(self.into())
    }

    fn err(self) -> ExecuteResult {
        Err(self)
    }
}
