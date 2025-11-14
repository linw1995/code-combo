use std::{process::Output, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use snafu::{Whatever, prelude::*};
use tokio::{
    process::Command,
    time::{error::Elapsed, timeout as tokio_timeout},
};

use super::{ExecuteResult, Tool};

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

#[derive(Snafu, Debug)]
pub enum BashError {
    #[snafu(display("{source}"))]
    Json { source: serde_json::Error },
    #[snafu(display("{source}"))]
    Execute { source: std::io::Error },
    #[snafu(display("{source}"))]
    Timeout { source: Elapsed },
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

    async fn execute(&self, input: Value) -> Result<ExecuteResult, Whatever> {
        let BashInput { command, timeout } = serde_json::from_value(input)
            .context(JsonSnafu)
            .whatever_context("deserialize input of tool error")?;

        let result = tokio_timeout(
            Duration::from_millis(timeout),
            Command::new("bash")
                .arg("-c")
                .arg(command)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context(TimeoutSnafu)
        .whatever_context("executing command timeout")?;

        let Output {
            status,
            stdout,
            stderr,
        } = result
            .context(ExecuteSnafu)
            .whatever_context("executing command error")?;
        let output = BashOutput {
            exit_code: status.code().unwrap_or(255) as u8,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        };

        let is_error = output.exit_code != 0;
        let output = serde_json::to_value(output)
            .context(JsonSnafu)
            .whatever_context("serialize output of tool error")?;
        Ok(ExecuteResult { output, is_error })
    }
}
