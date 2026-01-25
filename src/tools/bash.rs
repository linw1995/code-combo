use std::{sync::OnceLock, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::Mutex, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{ExecuteResult, Final, Input, Tool};
use crate::{
    MCP_SOCKET_ENV, McpSocketServer, SessionEnv, default_config_dir,
    exec::{ChunkConfig, ExecCommand, OutputChunk, ProcessEvent, StreamKind},
    load_config_with_overrides, workspace_config_path,
};

#[derive(Default)]
pub struct BashTool {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BashInput {
    pub command: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout: u64,
}

impl BashInput {
    pub fn new(command: String) -> Self {
        Self {
            command,
            timeout: default_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BashOutput {
    pub exit_code: u8,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub timed_out: bool,
}

static MCP_ENV_STATE: OnceLock<Mutex<Option<McpEnvState>>> = OnceLock::new();

fn default_timeout_ms() -> u64 {
    600_000
}

pub(crate) fn extra_envs_for_bash_input(input: &Input<'_>) -> Vec<(String, String)> {
    let Input::Starter(input) = input else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_value::<BashInput>(input.clone()) else {
        return Vec::new();
    };
    extra_envs_for_command(&parsed.command)
}

fn extra_envs_for_command(_command: &str) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    if let Ok(value) = std::env::var("COCO_SESSION_SOCK")
        && !value.is_empty()
    {
        envs.push(("COCO_SESSION_SOCK".to_string(), value));
    }
    if let Ok(value) = std::env::var("COCO_TUI_BIN")
        && !value.is_empty()
    {
        envs.push(("COCO_TUI_BIN".to_string(), value));
    }
    envs
}

pub const BASH_TOOL_NAME: &str = "bash";

pub async fn run_bash_chunked<'a, F>(
    input: Input<'a>,
    extra_envs: &[(String, String)],
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
                timed_out: false,
            };
            let output = serde_json::to_value(&output)
                .map_err(|err| format!("Failed to serialize tool output: {err}"))?;
            return Final::Json(output).err();
        }
    };

    let output = match run_bash_chunked_raw(input, extra_envs, cancel_token, |chunk| {
        on_chunk(chunk)
    })
    .await
    {
        Ok(output) => output,
        Err(err) => BashOutput {
            exit_code: 255,
            stdout: String::new(),
            stderr: err,
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
    extra_envs: &[(String, String)],
    cancel_token: CancellationToken,
    mut on_chunk: F,
) -> Result<BashOutput, String>
where
    F: FnMut(&OutputChunk) + Send,
{
    let BashInput { command, timeout } = input;

    let argv = vec!["bash".to_string(), "-c".to_string(), command];
    let envs = match prepare_mcp_envs().await {
        Ok(value) => value,
        Err(err) => {
            warn!(?err, "Failed to prepare MCP env for bash tool");
            Vec::new()
        }
    };
    let mut envs = envs;
    envs.extend(extra_envs.iter().cloned());
    let mut proc = ExecCommand::from_argv(argv)
        .remove_env_prefix("COCO_")
        .envs(envs)
        .spawn_chunked(ChunkConfig {
            interval: Duration::ZERO,
        })
        .map_err(|err| format!("Failed to execute command: {err}"))?;

    let mut output = BashOutput {
        exit_code: 255,
        stdout: String::new(),
        stderr: String::new(),
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
                        for line in &chunk.lines {
                            match chunk.stream {
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
                        on_chunk(&chunk);
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
        let extra_envs = extra_envs_for_bash_input(&input);
        run_bash_chunked(input, &extra_envs, CancellationToken::new(), |_| {}).await
    }
}

struct McpEnvState {
    _env: SessionEnv,
    _server: McpSocketServer,
    envs: Vec<(String, String)>,
}

pub async fn prepare_mcp_envs() -> Result<Vec<(String, String)>, String> {
    let state = MCP_ENV_STATE.get_or_init(|| Mutex::new(None));
    let mut state = state.lock().await;
    if let Some(existing) = state.as_ref() {
        return Ok(existing.envs.clone());
    }

    let mut config = if let Some(config) = crate::global::config().await {
        config
    } else {
        let config_dir = default_config_dir();
        let config_path = config_dir.join("config.toml");
        if !config_path.exists() {
            return Ok(Vec::new());
        }
        let workspace_path = workspace_config_path();

        load_config_with_overrides(&config_path, &config_dir, Some(&workspace_path))
            .map_err(|err| format!("Failed to parse config file: {err}"))?
    };
    if config.config_dir.as_os_str().is_empty() {
        config.config_dir = default_config_dir();
    }
    let Some(mut mcp) = config.mcp else {
        return Ok(Vec::new());
    };

    let env = SessionEnv::builder()
        .socket_env_name(MCP_SOCKET_ENV)
        .socket_name("coco-mcp.sock")
        .build()
        .map_err(|err| format!("Failed to build mcp env: {err}"))?;
    mcp.socket_path = env.socket_path().to_path_buf();
    let server = McpSocketServer::start(mcp, &config.config_dir)
        .await
        .map_err(|err| format!("Failed to start mcp server: {err}"))?;
    let envs: Vec<_> = env
        .envs()
        .into_iter()
        .map(|(key, value)| {
            (
                key.to_string_lossy().to_string(),
                value.to_string_lossy().to_string(),
            )
        })
        .collect();
    *state = Some(McpEnvState {
        _env: env,
        _server: server,
        envs: envs.clone(),
    });
    Ok(envs)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::tools::Output;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_output_split_streams() {
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
