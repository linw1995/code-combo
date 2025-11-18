use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{process::Command, time::timeout as tokio_timeout};

use super::{ExecuteResult, Output, Tool};

#[derive(Default)]
pub struct BashTool {}

#[derive(Debug, Serialize, Deserialize)]
pub struct BashInput {
    pub command: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BashOutput {
    pub exit_code: u8,
    pub stdout: String,
    pub stderr: String,
}

fn default_timeout_ms() -> u64 {
    600_000
}

pub const BASH_TOOL_NAME: &str = "bash";

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        BASH_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "A Bash for excuting command"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to execute"},
                "timeout": {"type": "number", "description": "Optional timeout in milliseconds", "max": 600000}
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value) -> ExecuteResult {
        let BashInput { command, timeout } = serde_json::from_value(input)
            .map_err(|err| format!("failed to deserialize tool input: {err}"))?;

        let result = tokio_timeout(
            Duration::from_millis(timeout),
            Command::new("bash")
                .arg("-c")
                .arg(command)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|err| format!("execeed command executing timeout: {err}"))?;

        let std::process::Output {
            status,
            stdout,
            stderr,
        } = result.map_err(|err| format!("failed to execute command: {err}"))?;

        let output = BashOutput {
            exit_code: status.code().unwrap_or(255) as u8,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        };

        let exit_code = output.exit_code;
        let output = serde_json::to_value(output)
            .map_err(|err| format!("failed to serialize tool output: {err}"))?;
        let output = Output::from(output);
        if exit_code == 0 {
            output.ok()
        } else {
            output.err()
        }
    }
}
