use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::{ExecuteResult, Final, Input, Tool};
use crate::exec::{ChunkConfig, ExecCommand, OutputChunk, ProcessEvent, StreamKind};

#[derive(Default)]
pub struct BashTool {}

#[derive(Debug, Serialize, Deserialize)]
pub struct BashInput {
    pub command: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashOutput {
    pub exit_code: u8,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub chunks: Vec<OutputChunk>,
    #[serde(default)]
    pub timed_out: bool,
}

fn default_timeout_ms() -> u64 {
    600_000
}

pub const BASH_TOOL_NAME: &str = "bash";

pub async fn run_bash_chunked<'a, F>(
    input: Input<'a>,
    cancel_token: CancellationToken,
    mut on_chunk: F,
) -> ExecuteResult
where
    F: FnMut(&OutputChunk) + Send,
{
    let Input::Starter(input) = input else {
        return err_msg!("Input should be Starter variant, not other variants");
    };
    let input: BashInput = match serde_json::from_value(input) {
        Ok(value) => value,
        Err(err) => {
            let output = BashOutput {
                exit_code: 255,
                stdout: String::new(),
                stderr: format!("Invalid input format: {err}"),
                chunks: Vec::new(),
                timed_out: false,
            };
            let output = serde_json::to_value(&output)
                .map_err(|err| format!("Failed to serialize tool output: {err}"))?;
            return Final::Json(output).err();
        }
    };

    let output = match run_bash_chunked_raw(input, cancel_token, |chunk| on_chunk(chunk)).await {
        Ok(output) => output,
        Err(err) => BashOutput {
            exit_code: 255,
            stdout: String::new(),
            stderr: err,
            chunks: Vec::new(),
            timed_out: false,
        },
    };

    let exit_code = output.exit_code;
    let output = serde_json::to_value(&output)
        .map_err(|err| format!("Failed to serialize tool output: {err}"))?;
    let output = Final::from(output);
    if exit_code == 0 {
        output.ok()
    } else {
        output.err()
    }
}

async fn run_bash_chunked_raw<F>(
    input: BashInput,
    cancel_token: CancellationToken,
    mut on_chunk: F,
) -> Result<BashOutput, String>
where
    F: FnMut(&OutputChunk) + Send,
{
    let BashInput { command, timeout } = input;

    let argv = vec!["bash".to_string(), "-c".to_string(), command];
    let mut proc = ExecCommand::from_argv(argv)
        .spawn_chunked(ChunkConfig {
            interval: Duration::ZERO,
        })
        .map_err(|err| format!("Failed to execute command: {err}"))?;

    let mut output = BashOutput {
        exit_code: 255,
        stdout: String::new(),
        stderr: String::new(),
        chunks: Vec::new(),
        timed_out: false,
    };

    let timeout_sleep = sleep(Duration::from_millis(timeout));
    tokio::pin!(timeout_sleep);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                output.exit_code = 130;
                proc.killer.kill();
                break;
            }
            _ = &mut timeout_sleep => {
                output.timed_out = true;
                output.exit_code = 124;
                proc.killer.kill();
                proc.abort();
                break;
            }
            ev = proc.events.next() => {
                let Some(ev) = ev else {
                    break;
                };
                match ev {
                    ProcessEvent::Started { .. } => (),
                    ProcessEvent::Chunk(chunk) => {
                        output.chunks.push(chunk);
                        let last = output.chunks.last().expect("chunk pushed");
                        for line in &last.lines {
                            match last.stream {
                                StreamKind::Stdout => {
                                    output.stdout.push_str(line);
                                    output.stdout.push('\n');
                                }
                                StreamKind::Stderr => {
                                    output.stderr.push_str(line);
                                    output.stderr.push('\n');
                                }
                            }
                        }
                        on_chunk(last);
                    }
                    ProcessEvent::Exited { exit_code, .. } => {
                        output.exit_code = exit_code.unwrap_or(255) as u8;
                        break;
                    }
                }
            }
        }
    }

    if !output.timed_out {
        let status = proc
            .wait()
            .await
            .map_err(|err| format!("Failed to execute command: {err}"))?;
        output.exit_code = status.code().unwrap_or(255) as u8;
    }

    Ok(output)
}

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

    async fn execute<'a>(&self, input: Input<'a>) -> ExecuteResult {
        run_bash_chunked(input, CancellationToken::new(), |_| {}).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::tools::Output;

    use super::*;

    fn stream_lines(output: &BashOutput, stream: StreamKind) -> Vec<&str> {
        output
            .chunks
            .iter()
            .filter(|c| c.stream == stream)
            .flat_map(|c| c.lines.iter())
            .map(|s| s.as_str())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_output_contains_chunks_and_split_streams() {
        let tool = BashTool::default();
        let input = Input::Starter(json!({
            "command": "printf \"out1\\nout2\\n\"; printf \"err1\\nerr2\\n\" 1>&2",
            "timeout": 60_000,
        }));

        let output = tool.execute(input).await.unwrap();
        let Output::Final(Final::Json(value)) = output else {
            panic!("expected JSON Final output");
        };
        let output: BashOutput = serde_json::from_value(value).unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "out1\nout2\n");
        assert_eq!(output.stderr, "err1\nerr2\n");
        assert!(!output.chunks.is_empty());
        assert_eq!(
            stream_lines(&output, StreamKind::Stdout),
            vec!["out1", "out2"]
        );
        assert_eq!(
            stream_lines(&output, StreamKind::Stderr),
            vec!["err1", "err2"]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_timeout_sets_timed_out_and_nonzero_exit() {
        let tool = BashTool::default();
        let input = Input::Starter(json!({
            "command": "sleep 10; echo out",
            "timeout": 10,
        }));

        let err = tool.execute(input).await.unwrap_err();
        let Final::Json(value) = err else {
            panic!("expected JSON Final error output");
        };
        let output: BashOutput = serde_json::from_value(value).unwrap();

        assert!(output.timed_out);
        assert_eq!(output.exit_code, 124);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_invalid_input_returns_json_error() {
        let tool = BashTool::default();
        let input = Input::Starter(json!({
            "timeout": "bad",
        }));

        let err = tool.execute(input).await.unwrap_err();
        let Final::Json(value) = err else {
            panic!("expected JSON Final error output");
        };
        let output: BashOutput = serde_json::from_value(value).unwrap();

        assert_eq!(output.exit_code, 255);
        assert!(output.stderr.contains("Invalid input format"));
    }
}
