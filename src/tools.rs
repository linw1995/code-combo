use async_trait::async_trait;
use serde_json::{Value, json};
use snafu::Whatever;

mod bash;

pub use bash::BashTool;

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
    async fn execute(&self, input: Value) -> Result<ExecuteResult, Whatever>;
}

#[derive(Debug, Clone)]
pub struct ExecuteResult {
    pub output: Value,
    pub is_error: bool,
}
