//! Run Combo Tool - Executes combo scripts within agent context.
//!
//! This tool allows the agent to execute combo scripts that have been
//! discovered during startup. Combos are executable scripts that can
//! perform complex operations with recorded tool calls.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    BASH_TOOL_NAME, BashInput, ExecuteResult, Final, Input, Output, Tool, prepare_mcp_envs,
    run_bash_chunked,
};
use crate::{
    Agent, Block, ChatStreamUpdate, Combo, Config, Content, Message, OutputChunk, PromptSchema,
    SessionEnv, Starter, StarterCommand, StarterError, StarterEvent, ThinkingConfig, ToolUse,
    bash_unsafe_ranges, bash_unsafe_reason, discover_starters, exec::StreamKind,
    parse_primary_command, workspace_dir,
};

pub const RUN_COMBO_TOOL_NAME: &str = "run_combo";

type ComboEventCallback = Arc<std::sync::Mutex<Box<dyn FnMut(&ComboEvent) + Send>>>;

/// Input parameters for the run_combo tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunComboInput {
    /// Name of the combo to execute.
    pub combo_name: String,
    /// Arguments passed to the combo starter.
    #[serde(default, deserialize_with = "deserialize_combo_args")]
    pub args: Vec<String>,
}

/// Output from the run_combo tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunComboOutput {
    /// Whether the combo completed successfully.
    pub success: bool,
    /// Summary of the combo execution.
    pub summary: String,
    /// Number of tool calls made during execution.
    pub tool_calls: usize,
    /// Optional error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboStreamKind {
    Plain,
    Thinking,
}

/// Event emitted during combo execution.
#[derive(Debug, Clone)]
pub enum ComboEvent {
    /// Combo not found.
    NotFound {
        /// Combo name.
        name: String,
    },
    /// Combo is executing.
    Executing {
        /// Combo name.
        name: String,
        /// Command line with args.
        command_line: String,
    },
    /// Regular output chunk (stdout/stderr).
    Output {
        /// Combo name.
        name: String,
        /// Output chunk.
        chunk: OutputChunk,
    },
    /// Tool record started.
    RecordStart {
        /// Combo name.
        name: String,
        /// Tool use info.
        tool_use: crate::ToolUse,
    },
    /// Tool record output.
    RecordOutput {
        /// Combo name.
        name: String,
        /// Tool use ID.
        tool_use_id: String,
        /// Output chunk.
        chunk: OutputChunk,
    },
    /// Tool record ended.
    RecordEnd {
        /// Combo name.
        name: String,
        /// Tool use ID.
        tool_use_id: String,
        /// Whether the tool failed.
        is_error: bool,
        /// Tool output.
        output: Final,
    },
    /// Prompt message from combo.
    Prompt {
        /// Combo name.
        name: String,
        /// Prompt text.
        prompt: String,
        /// Optional thinking config.
        thinking: Option<ThinkingConfig>,
    },
    /// Prompt stream update for combo reply.
    PromptStream {
        /// Combo name.
        name: String,
        /// Stream index.
        index: usize,
        /// Stream kind.
        kind: ComboStreamKind,
        /// Streamed text.
        text: String,
    },
    /// Reply tool use from prompt.
    ReplyToolUse {
        /// Combo name.
        name: String,
        /// Tool use info.
        tool_use: ToolUse,
        /// Thinking blocks.
        thinking: Vec<String>,
        /// Whether this is an offload reply (executed via bash).
        offload: bool,
    },
    /// Reply tool result for offload.
    ReplyToolResult {
        /// Combo name.
        name: String,
        /// Tool use ID.
        tool_use_id: String,
        /// Whether the tool failed.
        is_error: bool,
        /// Tool output.
        output: Final,
    },
    /// Reply tool error.
    ReplyToolError {
        /// Error message.
        message: String,
    },
    /// Combo execution finished.
    Executed {
        /// Combo name.
        name: String,
        /// Starter summary.
        starter: Starter,
        /// Exit code.
        exit_code: Option<i32>,
    },
    /// Combo execution was cancelled.
    Cancelled {
        /// Combo name if available.
        name: Option<String>,
    },
}

/// Information about a discovered combo.
#[derive(Debug, Clone)]
pub struct ComboInfo {
    /// Path to the combo executable.
    pub path: String,
    /// Combo metadata.
    pub combo: Combo,
}

/// Context needed to create and run combo instances.
#[derive(Clone)]
pub struct RunComboContext {
    /// Available combos discovered at startup.
    pub combos: Vec<ComboInfo>,
    /// Environment variables for combo execution.
    pub envs: Vec<(String, String)>,
    /// Config for combo reply agent.
    pub config: Config,
    /// System prompt used for combo reply.
    pub system_prompt: String,
    /// Optional model override for combo reply.
    pub model_override: Option<String>,
    /// Whether thinking is enabled for combo reply.
    pub thinking_enabled: bool,
}

/// Tool for executing combo scripts.
pub struct RunComboTool {
    context: Arc<Mutex<RunComboContext>>,
}

impl RunComboTool {
    /// Create a new RunComboTool with the given context.
    pub fn new(context: RunComboContext) -> Self {
        Self {
            context: Arc::new(Mutex::new(context)),
        }
    }

    /// Create a new RunComboTool with a shared context.
    /// This allows external code to update the combo list after tool creation.
    pub fn new_with_shared_context(context: Arc<Mutex<RunComboContext>>) -> Self {
        Self { context }
    }

    /// Get the shared context.
    pub fn context(&self) -> Arc<Mutex<RunComboContext>> {
        self.context.clone()
    }
}

#[async_trait]
impl Tool for RunComboTool {
    fn name(&self) -> &'static str {
        RUN_COMBO_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Execute a combo script to perform predefined operations."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "combo_name": {
                    "type": "string",
                    "description": "Name of the combo to execute"
                },
                "args": {
                    "type": "string",
                    "description": "Arguments passed to the combo starter"
                }
            },
            "required": ["combo_name"]
        })
    }

    async fn execute<'a>(&self, input: Input<'a>) -> ExecuteResult {
        run_combo(
            self.context.clone(),
            input,
            CancellationToken::new(),
            |_| {},
        )
        .await
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

/// Execute the run_combo tool with event callback support.
pub async fn run_combo<F>(
    context: Arc<Mutex<RunComboContext>>,
    input: Input<'_>,
    cancel_token: CancellationToken,
    on_event: F,
) -> ExecuteResult
where
    F: FnMut(&ComboEvent) + Send + 'static,
{
    let Input::Starter(input) = input else {
        return err_msg!("Input should be Starter variant");
    };

    let input: RunComboInput = match serde_json::from_value(input) {
        Ok(v) => v,
        Err(e) => {
            return Final::from(json!({
                "success": false,
                "summary": "",
                "tool_calls": 0,
                "error": format!("Invalid input: {}", e)
            }))
            .err();
        }
    };

    debug!(combo_name = %input.combo_name, "Starting combo execution");

    // Wrap callback early so errors can emit events.
    let on_event_boxed: ComboEventCallback = Arc::new(std::sync::Mutex::new(Box::new(on_event)));

    let (envs, config, system_prompt, model_override, thinking_enabled) = {
        let ctx = context.lock().await;
        (
            ctx.envs.clone(),
            ctx.config.clone(),
            ctx.system_prompt.clone(),
            ctx.model_override.clone(),
            ctx.thinking_enabled,
        )
    };

    let mut combo_info = {
        let ctx = context.lock().await;
        ctx.combos
            .iter()
            .find(|c| c.combo.metadata.name == input.combo_name)
            .cloned()
    };

    if combo_info.is_none() {
        let discovered = discover_combo_infos(&config, cancel_token.clone()).await;
        if discovered.cancelled || cancel_token.is_cancelled() {
            if let Ok(mut f) = on_event_boxed.lock() {
                (*f)(&ComboEvent::Cancelled {
                    name: Some(input.combo_name.clone()),
                });
            }
            return Final::from(json!({
                "success": false,
                "summary": "",
                "tool_calls": 0,
                "error": "Cancelled"
            }))
            .err();
        }
        {
            let mut ctx = context.lock().await;
            ctx.combos = discovered.combos.clone();
        }
        combo_info = discovered
            .combos
            .iter()
            .find(|c| c.combo.metadata.name == input.combo_name)
            .cloned();
    }

    let Some(combo_info) = combo_info else {
        let available: Vec<String> = {
            let ctx = context.lock().await;
            ctx.combos
                .iter()
                .map(|c| c.combo.metadata.name.clone())
                .collect()
        };
        let missing_name = input.combo_name.clone();
        if let Ok(mut f) = on_event_boxed.lock() {
            (*f)(&ComboEvent::NotFound {
                name: missing_name.clone(),
            });
        }
        return Final::from(json!({
            "success": false,
            "summary": "",
            "tool_calls": 0,
            "error": format!(
                "Combo '{}' not found. Available: {:?}",
                missing_name, available
            )
        }))
        .err();
    };

    let combo_path = combo_info.path.clone();
    let combo_name = combo_info.combo.metadata.name.clone();
    let combo_args = input.args.clone();

    // Execute combo
    let result = execute_combo(
        combo_path,
        combo_name,
        combo_args,
        envs,
        config,
        system_prompt,
        model_override,
        thinking_enabled,
        cancel_token,
        on_event_boxed,
    )
    .await;

    match result {
        Ok(output) => {
            debug!(
                success = output.success,
                tool_calls = output.tool_calls,
                summary_len = output.summary.len(),
                "run_combo completed"
            );
            let json_output = serde_json::to_value(&output)
                .unwrap_or_else(|_| json!({"success": false, "error": "Serialization failed"}));
            if output.success {
                Final::from(json_output).ok()
            } else {
                Final::from(json_output).err()
            }
        }
        Err(e) => {
            debug!(error = %e, "run_combo failed");
            Final::from(json!({
                "success": false,
                "summary": "",
                "tool_calls": 0,
                "error": e
            }))
            .err()
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_combo(
    combo_path: String,
    combo_name: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    config: Config,
    system_prompt: String,
    model_override: Option<String>,
    thinking_enabled: bool,
    cancel_token: CancellationToken,
    on_event: ComboEventCallback,
) -> Result<RunComboOutput, String> {
    let session_env = SessionEnv::builder()
        .build()
        .map_err(|e| format!("Failed to create session env: {}", e))?;
    let session_socket_path = session_env.socket_path().to_path_buf();
    let mut exec_envs = match prepare_mcp_envs().await {
        Ok(envs) => envs,
        Err(err) => {
            warn!(?err, "Failed to prepare MCP env for combo execution");
            Vec::new()
        }
    };
    exec_envs.extend(envs);

    let mut reply_agent = build_reply_agent(
        config,
        system_prompt.clone(),
        model_override,
        thinking_enabled,
    );

    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    };

    let on_event_for_emit = on_event.clone();

    // Buffers for aggregating output into lines
    let stdout_buffer = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_buffer = Arc::new(std::sync::Mutex::new(String::new()));

    let mut tool_calls = 0;
    let mut summary_parts: Vec<String> = Vec::new();
    let mut exit_code: Option<i32> = None;
    let mut failed = false;
    let mut cancelled = false;

    let command_line = format_command_line(&combo_path, &args);
    // Emit executing event
    emit_combo_event(
        &on_event_for_emit,
        ComboEvent::Executing {
            name: combo_name.clone(),
            command_line,
        },
    );

    let mut execution = StarterCommand::new(&combo_path)
        .args(args)
        .envs(exec_envs)
        .session_env(session_env)
        .execute();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled(), if !cancelled => {
                cancelled = true;
                execution.cancel();
            }
            event = futures_util::StreamExt::next(&mut execution) => {
                let Some(event) = event else { break };

                match event {
                    StarterEvent::Started { .. } => {}
                    StarterEvent::Output { chunk } => {
                        emit_combo_event(&on_event_for_emit, ComboEvent::Output {
                            name: combo_name.clone(),
                            chunk: chunk.clone(),
                        });

                        // Aggregate output for summary
                        for line in &chunk.lines {
                            if !line.trim().is_empty() {
                                summary_parts.push(line.clone());
                            }
                        }
                    }
                    StarterEvent::RecordStart { tool_use } => {
                        tool_calls += 1;
                        reply_agent
                            .append_message(build_tool_use_message(&tool_use))
                            .await;
                        emit_combo_event(&on_event_for_emit, ComboEvent::RecordStart {
                            name: combo_name.clone(),
                            tool_use,
                        });
                    }
                    StarterEvent::RecordOutput { tool_use_id, chunk } => {
                        let tool_use_id_summary = tool_use_id.clone();
                        emit_combo_event(&on_event_for_emit, ComboEvent::RecordOutput {
                            name: combo_name.clone(),
                            tool_use_id,
                            chunk: chunk.clone(),
                        });
                        for line in &chunk.lines {
                            if !line.trim().is_empty() {
                                summary_parts.push(format!("[{}] {}", tool_use_id_summary, line));
                            }
                        }
                    }
                    StarterEvent::RecordEnd {
                        tool_use_id,
                        is_error,
                        output,
                    } => {
                        reply_agent
                            .append_message(build_tool_result_message(
                                &tool_use_id,
                                is_error,
                                &output,
                            ))
                            .await;
                        emit_combo_event(&on_event_for_emit, ComboEvent::RecordEnd {
                            name: combo_name.clone(),
                            tool_use_id,
                            is_error,
                            output: output.clone(),
                        });
                        if is_error {
                            failed = true;
                        }
                    }
                    StarterEvent::Prompt { prompt } => {
                        reply_agent
                            .append_message(build_prompt_message(&prompt))
                            .await;
                        emit_combo_event(&on_event_for_emit, ComboEvent::Prompt {
                            name: combo_name.clone(),
                            prompt: prompt.clone(),
                            thinking: None,
                        });
                        summary_parts.push(format!("[Prompt] {}", prompt));
                    }
                    StarterEvent::PromptRequest {
                        prompt,
                        schemas,
                        responder,
                        thinking,
                    } => {
                        reply_agent
                            .append_message(build_prompt_message(&prompt))
                            .await;
                        emit_combo_event(&on_event_for_emit, ComboEvent::Prompt {
                            name: combo_name.clone(),
                            prompt: prompt.clone(),
                            thinking: thinking.clone(),
                        });
                        summary_parts.push(format!("[Prompt] {}", prompt));

                        if reply_agent.offload_combo_reply() {
                            let result = handle_offload_combo_reply_with_retry(
                                &mut reply_agent,
                                &schemas,
                                &combo_name,
                                cancel_token.clone(),
                                session_socket_path.clone(),
                                &on_event_for_emit,
                            )
                            .await;
                            if let Err(err) = result {
                                emit_combo_event(
                                    &on_event_for_emit,
                                    ComboEvent::ReplyToolError {
                                        message: err.to_string(),
                                    },
                                );
                            }
                        } else {
                            let disable_stream =
                                reply_agent.disable_stream_for_current_model();
                            let mut streamed_thinking = false;
                            let reply = if cancel_token.is_cancelled() {
                                Err("prompt reply cancelled".to_string())
                            } else if disable_stream {
                                reply_agent
                                    .reply_prompt_with_thinking(
                                        &system_prompt,
                                        schemas.clone(),
                                        thinking.clone(),
                                    )
                                    .await
                                    .map_err(|err| err.to_string())
                            } else {
                                let stream_name = combo_name.clone();
                                let thinking_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
                                let thinking_seen_stream = thinking_seen.clone();
                                let on_event_stream = on_event_for_emit.clone();
                                let reply = reply_agent
                                    .reply_prompt_stream_with_thinking(
                                        &system_prompt,
                                        schemas.clone(),
                                        thinking.clone(),
                                        cancel_token.clone(),
                                        move |update| {
                                            let (index, kind, text) = match update {
                                                ChatStreamUpdate::Plain { index, text } => {
                                                    (index, ComboStreamKind::Plain, text)
                                                }
                                                ChatStreamUpdate::Thinking { index, text } => {
                                                    thinking_seen_stream
                                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                                    (index, ComboStreamKind::Thinking, text)
                                                }
                                            };
                                            emit_combo_event(
                                                &on_event_stream,
                                                ComboEvent::PromptStream {
                                                    name: stream_name.clone(),
                                                    index,
                                                    kind,
                                                    text,
                                                },
                                            );
                                        },
                                    )
                                    .await
                                    .map_err(|err| err.to_string());
                                streamed_thinking =
                                    thinking_seen.load(std::sync::atomic::Ordering::Relaxed);
                                reply
                            };
                            if let Ok(reply) = &reply {
                                let thinking_blocks = if streamed_thinking {
                                    Vec::new()
                                } else {
                                    reply.thinking.clone()
                                };
                                emit_combo_event(
                                    &on_event_for_emit,
                                    ComboEvent::ReplyToolUse {
                                        name: combo_name.clone(),
                                        tool_use: reply.tool_use.clone(),
                                        thinking: thinking_blocks,
                                        offload: false,
                                    },
                                );
                            }
                            let response = reply.map(|reply| reply.response);
                            if let Err(err) = &response {
                                emit_combo_event(
                                    &on_event_for_emit,
                                    ComboEvent::ReplyToolError {
                                        message: err.clone(),
                                    },
                                );
                            }
                            if let Err(err) = responder.send(response) {
                                emit_combo_event(
                                    &on_event_for_emit,
                                    ComboEvent::ReplyToolError { message: err },
                                );
                            }
                        }
                    }
                    StarterEvent::Finished { exit_code: code } => {
                        exit_code = code;
                    }
                    StarterEvent::Cancelled => {
                        cancelled = true;
                    }
                    StarterEvent::Failed { reason } => {
                        failed = true;
                        summary_parts.push(format!("[Failed] {}", reason));
                    }
                }
            }
        }
    }

    // Wait for execution to complete
    let starter = match execution.wait().await {
        Ok(starter) => starter,
        Err(err) => {
            let reason = format!("starter join error: {err}");
            let starter = Starter {
                path: combo_path.clone(),
                combo: Err(StarterError::Invalid {
                    reason: reason.clone(),
                }),
            };
            emit_combo_event(
                &on_event_for_emit,
                ComboEvent::Executed {
                    name: combo_name.clone(),
                    starter,
                    exit_code,
                },
            );
            return Err(format!("Join error: {}", err));
        }
    };

    // Flush remaining buffers
    flush_buffer(
        &combo_name,
        &stdout_buffer,
        StreamKind::Stdout,
        &on_event,
        now_ms,
    );
    flush_buffer(
        &combo_name,
        &stderr_buffer,
        StreamKind::Stderr,
        &on_event,
        now_ms,
    );

    if cancelled || matches!(&starter.combo, Err(StarterError::Cancelled)) {
        emit_combo_event(
            &on_event_for_emit,
            ComboEvent::Cancelled {
                name: Some(combo_name.clone()),
            },
        );
        return Ok(RunComboOutput {
            success: false,
            summary: "Combo execution was cancelled".to_string(),
            tool_calls,
            error: Some("Cancelled".to_string()),
        });
    }

    emit_combo_event(
        &on_event_for_emit,
        ComboEvent::Executed {
            name: combo_name.clone(),
            starter: starter.clone(),
            exit_code,
        },
    );

    // Check combo result
    if let Err(e) = starter.combo {
        return Ok(RunComboOutput {
            success: false,
            summary: summary_parts.join("\n"),
            tool_calls,
            error: Some(format!("Combo error: {}", e)),
        });
    }

    let success = !failed && exit_code.map(|c| c == 0).unwrap_or(true);
    let summary = if summary_parts.is_empty() {
        format!(
            "Combo '{}' completed with {} tool call(s)",
            combo_name, tool_calls
        )
    } else {
        // Take last few lines as summary
        let max_lines = 10;
        let start = summary_parts.len().saturating_sub(max_lines);
        summary_parts[start..].join("\n")
    };

    Ok(RunComboOutput {
        success,
        summary,
        tool_calls,
        error: if failed {
            Some("One or more tool calls failed".to_string())
        } else {
            None
        },
    })
}

fn emit_combo_event(on_event: &ComboEventCallback, event: ComboEvent) {
    if let Ok(mut f) = on_event.lock() {
        (*f)(&event);
    }
}

fn deserialize_combo_args<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawArgs {
        List(Vec<String>),
        Single(String),
    }

    let raw = Option::<RawArgs>::deserialize(deserializer)?;
    Ok(match raw {
        None => Vec::new(),
        Some(RawArgs::List(items)) => items,
        Some(RawArgs::Single(value)) => vec![value],
    })
}

fn build_reply_agent(
    config: Config,
    system_prompt: String,
    model_override: Option<String>,
    thinking_enabled: bool,
) -> Agent {
    let mut agent = Agent::new(config);
    agent.set_system_prompt(&system_prompt);
    agent.set_model_override(model_override);
    agent.set_thinking_enabled(thinking_enabled);
    agent
}

fn build_tool_use_message(tool_use: &ToolUse) -> Message {
    Message::assistant(Content::Multiple(vec![Block::tool_use(
        &tool_use.id,
        &tool_use.name,
        tool_use.input.clone(),
    )]))
}

fn build_tool_result_message(tool_use_id: &str, is_error: bool, output: &Final) -> Message {
    Message::user(Content::Multiple(vec![Block::tool_result(
        tool_use_id,
        Some(is_error),
        final_to_tool_content(output),
    )]))
}

fn build_prompt_message(prompt: &str) -> Message {
    Message::user(Content::Text(prompt.to_string()))
}

const TOOL_RESULT_MAX_BYTES: usize = 80 * 1024;
const TOOL_RESULT_TRUNCATION_SUFFIX: &str = "\n... (truncated)";

fn final_to_tool_content(output: &Final) -> Content {
    let text = match output {
        Final::Json(value) => truncate_json_tool_output(value, TOOL_RESULT_MAX_BYTES)
            .unwrap_or_else(|| {
                let raw = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
                truncate_with_suffix(&raw, TOOL_RESULT_MAX_BYTES, TOOL_RESULT_TRUNCATION_SUFFIX)
            }),
        Final::Message(message) => truncate_with_suffix(
            message,
            TOOL_RESULT_MAX_BYTES,
            TOOL_RESULT_TRUNCATION_SUFFIX,
        ),
    };
    Content::Text(text)
}

fn truncate_json_tool_output(value: &Value, max_bytes: usize) -> Option<String> {
    let obj = value.as_object()?;
    let stdout_value = obj.get("stdout").and_then(|value| value.as_str());
    let stderr_value = obj.get("stderr").and_then(|value| value.as_str());
    if stdout_value.is_none() && stderr_value.is_none() {
        return None;
    }

    let serialized = serde_json::to_string(value).ok()?;
    if serialized.len() <= max_bytes {
        return Some(serialized);
    }

    let stdout = stdout_value.unwrap_or("");
    let stderr = stderr_value.unwrap_or("");
    let stdout_len = stdout.len();
    let stderr_len = stderr.len();

    let mut base = obj.clone();
    if stdout_value.is_some() {
        base.insert("stdout".to_string(), Value::String(String::new()));
    }
    if stderr_value.is_some() {
        base.insert("stderr".to_string(), Value::String(String::new()));
    }
    base.insert("_truncated".to_string(), Value::Bool(true));
    let base_text = serde_json::to_string(&Value::Object(base)).ok()?;
    if base_text.len() >= max_bytes {
        return Some(truncate_with_suffix(
            &base_text,
            max_bytes,
            TOOL_RESULT_TRUNCATION_SUFFIX,
        ));
    }

    let available = max_bytes - base_text.len();
    let total_len = stdout_len + stderr_len;
    let (mut stdout_budget, mut stderr_budget) = if total_len == 0 {
        (0, 0)
    } else if stderr_len == 0 {
        (available, 0)
    } else if stdout_len == 0 {
        (0, available)
    } else {
        let stdout_budget = available * stdout_len / total_len;
        let stderr_budget = available.saturating_sub(stdout_budget);
        (stdout_budget, stderr_budget)
    };

    let mut last_text = base_text;
    for _ in 0..5 {
        let mut out = obj.clone();
        if stdout_value.is_some() {
            let truncated = truncate_to_boundary(stdout, stdout_budget);
            out.insert("stdout".to_string(), Value::String(truncated.to_string()));
        }
        if stderr_value.is_some() {
            let truncated = truncate_to_boundary(stderr, stderr_budget);
            out.insert("stderr".to_string(), Value::String(truncated.to_string()));
        }
        out.insert("_truncated".to_string(), Value::Bool(true));
        let text = serde_json::to_string(&Value::Object(out)).ok()?;
        if text.len() <= max_bytes {
            return Some(text);
        }

        last_text = text;
        if stdout_budget == 0 && stderr_budget == 0 {
            break;
        }
        let overshoot = last_text.len().saturating_sub(max_bytes);
        if stdout_budget >= stderr_budget {
            stdout_budget = stdout_budget.saturating_sub(overshoot);
        } else {
            stderr_budget = stderr_budget.saturating_sub(overshoot);
        }
    }

    Some(truncate_with_suffix(
        &last_text,
        max_bytes,
        TOOL_RESULT_TRUNCATION_SUFFIX,
    ))
}

fn truncate_with_suffix(text: &str, max_bytes: usize, suffix: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }

    let suffix = if max_bytes <= suffix.len() {
        truncate_to_boundary(suffix, max_bytes)
    } else {
        suffix
    };
    if max_bytes <= suffix.len() {
        return suffix.to_string();
    }

    let keep_len = max_bytes - suffix.len();
    let prefix = truncate_to_boundary(text, keep_len);
    let mut out = String::with_capacity(max_bytes);
    out.push_str(prefix);
    out.push_str(suffix);
    out
}

fn truncate_to_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn build_offload_reply_directive(schemas: &[PromptSchema]) -> String {
    let field_args: Vec<String> = schemas
        .iter()
        .map(|schema| format!("--{}=<value>", schema.name))
        .collect();

    let field_descriptions: Vec<String> = schemas
        .iter()
        .map(|schema| format!("- --{}=<value>: {}", schema.name, schema.description))
        .collect();

    format!(
        r#"You must respond by calling the bash tool to execute the `coco reply` command.
Use this exact format:
```
coco reply {field_args}
```

Required fields:
{field_list}

The value should be properly shell-escaped if it contains special characters.
Do not output any other text or explanation. Only call the bash tool with the coco reply command."#,
        field_args = field_args.join(" "),
        field_list = field_descriptions.join("\n"),
    )
}

fn build_offload_reply_retry_directive(schemas: &[PromptSchema]) -> String {
    let directive = build_offload_reply_directive(schemas);
    format!("The previous response did not produce a valid coco reply. Retry.\n\n{directive}")
}

enum OffloadCommandKind {
    Coco,
    Safe,
    Unsafe,
}

fn classify_offload_command(command: &str) -> OffloadCommandKind {
    let is_coco_reply = is_coco_reply_command(command);
    if is_coco_reply {
        return OffloadCommandKind::Coco;
    }
    if is_safe_command(command) {
        return OffloadCommandKind::Safe;
    }
    OffloadCommandKind::Unsafe
}

fn build_offload_reply_guidance(schemas: &[PromptSchema], command: &str, executed: bool) -> String {
    let field_args: Vec<String> = schemas
        .iter()
        .map(|schema| format!("--{}=...", schema.name))
        .collect();
    let field_descriptions: Vec<String> = schemas
        .iter()
        .map(|schema| format!("- {}: {}", schema.name, schema.description))
        .collect();
    let status = if executed { "executed" } else { "blocked" };
    format!(
        "The previous tool call was {status} but did not use `coco reply` (command: {command}).\n\
You must call the bash tool with `coco reply` and only that command.\n\
Required fields:\n{field_list}\n\n\
Example:\n\
coco reply {field_args}",
        field_list = field_descriptions.join("\n"),
        field_args = field_args.join(" "),
    )
}

#[derive(Debug)]
enum ComboReplyError {
    Cancelled,
    ChatFailed { message: String },
    MissingBashToolUse,
    InvalidBashInput { message: String },
    UnexpectedCommand { command: String },
}

impl ComboReplyError {
    fn should_retry(&self) -> bool {
        matches!(
            self,
            ComboReplyError::MissingBashToolUse
                | ComboReplyError::InvalidBashInput { .. }
                | ComboReplyError::UnexpectedCommand { .. }
        )
    }
}

impl std::fmt::Display for ComboReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComboReplyError::Cancelled => write!(f, "prompt reply cancelled"),
            ComboReplyError::ChatFailed { message } => write!(f, "chat failed: {message}"),
            ComboReplyError::MissingBashToolUse => {
                write!(f, "LLM did not return a bash tool call for coco reply")
            }
            ComboReplyError::InvalidBashInput { message } => {
                write!(f, "failed to parse bash tool input: {message}")
            }
            ComboReplyError::UnexpectedCommand { command } => {
                write!(f, "expected coco reply command, got: {command}")
            }
        }
    }
}

impl std::error::Error for ComboReplyError {}

fn is_coco_command_name(name: &str) -> bool {
    name == "coco" || name.ends_with("/coco")
}

fn is_coco_reply_command(command: &str) -> bool {
    let summary = match parse_primary_command(command) {
        Ok(summary) => summary,
        Err(_) => return false,
    };
    if !is_coco_command_name(&summary.name) {
        return false;
    }
    matches!(summary.args.first(), Some(arg) if arg == "reply")
}

fn is_safe_command(command: &str) -> bool {
    let trimmed = command.trim();
    !trimmed.is_empty() && bash_unsafe_ranges(command).is_empty()
}

async fn handle_offload_combo_reply_with_retry(
    agent: &mut Agent,
    schemas: &[PromptSchema],
    combo_name: &str,
    cancel_token: CancellationToken,
    session_socket_path: PathBuf,
    on_event: &ComboEventCallback,
) -> Result<(), ComboReplyError> {
    let max_retries = agent.combo_reply_retries();
    let mut attempt = 0usize;
    loop {
        if cancel_token.is_cancelled() {
            return Err(ComboReplyError::Cancelled);
        }
        let directive = if attempt == 0 {
            build_offload_reply_directive(schemas)
        } else {
            build_offload_reply_retry_directive(schemas)
        };
        let response = handle_offload_combo_reply(
            agent,
            schemas,
            combo_name,
            cancel_token.clone(),
            session_socket_path.clone(),
            on_event,
            &directive,
        )
        .await;
        match response {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= max_retries || !err.should_retry() {
                    return Err(err);
                }
                attempt += 1;
            }
        }
    }
}

async fn handle_offload_combo_reply(
    agent: &mut Agent,
    schemas: &[PromptSchema],
    combo_name: &str,
    cancel_token: CancellationToken,
    session_socket_path: PathBuf,
    on_event: &ComboEventCallback,
    directive: &str,
) -> Result<(), ComboReplyError> {
    if cancel_token.is_cancelled() {
        return Err(ComboReplyError::Cancelled);
    }

    agent
        .append_message(Message::user(Content::Text(directive.to_string())))
        .await;

    let on_event_stream = on_event.clone();
    let stream_name = combo_name.to_string();
    let chat_response = agent
        .chat_stream_with_history(cancel_token.clone(), move |update| {
            let (index, kind, text) = match update {
                ChatStreamUpdate::Plain { index, text } => (index, ComboStreamKind::Plain, text),
                ChatStreamUpdate::Thinking { index, text } => {
                    (index, ComboStreamKind::Thinking, text)
                }
            };
            emit_combo_event(
                &on_event_stream,
                ComboEvent::PromptStream {
                    name: stream_name.clone(),
                    index,
                    kind,
                    text,
                },
            );
        })
        .await
        .map_err(|e| ComboReplyError::ChatFailed {
            message: e.to_string(),
        })?;

    let blocks = match &chat_response.message.content {
        Content::Multiple(blocks) => blocks.as_slice(),
        Content::Text(_) => &[],
    };

    let bash_tool_use = blocks
        .iter()
        .find_map(|block| {
            if let Block::ToolUse(tool_use) = block
                && tool_use.name == BASH_TOOL_NAME
            {
                return Some(tool_use.clone());
            }
            None
        })
        .ok_or(ComboReplyError::MissingBashToolUse)?;

    let bash_input: BashInput =
        serde_json::from_value(bash_tool_use.input.clone()).map_err(|err| {
            ComboReplyError::InvalidBashInput {
                message: err.to_string(),
            }
        })?;

    let original_command = bash_input.command.clone();
    let command_kind = classify_offload_command(&bash_input.command);

    emit_combo_event(
        on_event,
        ComboEvent::ReplyToolUse {
            name: combo_name.to_string(),
            tool_use: bash_tool_use.clone(),
            thinking: Vec::new(),
            offload: true,
        },
    );

    if matches!(command_kind, OffloadCommandKind::Unsafe) {
        let reason = match bash_unsafe_reason(&original_command) {
            Ok(_) => "command not allowlisted".to_string(),
            Err(reason) => reason,
        };
        let output = Final::Message(format!("command rejected: {reason}; expected coco reply"));
        agent
            .append_message(build_tool_result_message(&bash_tool_use.id, true, &output))
            .await;
        emit_combo_event(
            on_event,
            ComboEvent::ReplyToolResult {
                name: combo_name.to_string(),
                tool_use_id: bash_tool_use.id.clone(),
                is_error: true,
                output: output.clone(),
            },
        );
        let prompt = build_offload_reply_guidance(schemas, &original_command, false);
        agent
            .append_message(Message::user(Content::Text(prompt)))
            .await;
        return Err(ComboReplyError::UnexpectedCommand {
            command: original_command,
        });
    }

    let bash_input_value =
        serde_json::to_value(&bash_input).map_err(|err| ComboReplyError::InvalidBashInput {
            message: err.to_string(),
        })?;

    let extra_envs = vec![(
        "COCO_SESSION_SOCK".to_string(),
        session_socket_path.to_string_lossy().to_string(),
    )];
    let output = run_bash_chunked(
        Input::Starter(bash_input_value),
        &extra_envs,
        cancel_token.clone(),
        |_| {},
    )
    .await;

    if cancel_token.is_cancelled() {
        return Err(ComboReplyError::Cancelled);
    }

    let (output, is_error) = match output {
        Ok(Output::Final(output)) => (output, false),
        Ok(Output::TextEdit(_)) => (
            Final::Message("unexpected tool output from bash".to_string()),
            true,
        ),
        Err(output) => (output, true),
    };

    agent
        .append_message(build_tool_result_message(
            &bash_tool_use.id,
            is_error,
            &output,
        ))
        .await;
    emit_combo_event(
        on_event,
        ComboEvent::ReplyToolResult {
            name: combo_name.to_string(),
            tool_use_id: bash_tool_use.id.clone(),
            is_error,
            output: output.clone(),
        },
    );

    if matches!(command_kind, OffloadCommandKind::Safe) {
        let prompt = build_offload_reply_guidance(schemas, &original_command, true);
        agent
            .append_message(Message::user(Content::Text(prompt)))
            .await;
        return Err(ComboReplyError::UnexpectedCommand {
            command: original_command,
        });
    }

    Ok(())
}

struct ComboDiscoveryResult {
    combos: Vec<ComboInfo>,
    cancelled: bool,
}

async fn discover_combo_infos(
    config: &Config,
    cancel_token: CancellationToken,
) -> ComboDiscoveryResult {
    let combo_dirs = combo_discovery_dirs(config);
    let combo_dirs = combo_dirs.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let result = discover_starters(&combo_dirs, cancel_token).await;
    let combos = result
        .starters
        .into_iter()
        .filter_map(|starter| match starter.combo {
            Ok(combo) => Some(ComboInfo {
                path: starter.path,
                combo,
            }),
            Err(err) => {
                warn!(?starter.path, ?err, "Failed to load combo");
                None
            }
        })
        .collect::<Vec<_>>();
    ComboDiscoveryResult {
        combos,
        cancelled: result.cancelled,
    }
}

fn combo_discovery_dirs(config: &Config) -> Vec<PathBuf> {
    vec![workspace_dir().join(".coco/combos"), config.combo_dir()]
}

fn flush_buffer(
    combo_name: &str,
    buffer: &Arc<std::sync::Mutex<String>>,
    stream: StreamKind,
    on_event: &ComboEventCallback,
    now_ms: impl Fn() -> i64,
) {
    if let Ok(mut buf) = buffer.lock()
        && !buf.is_empty()
    {
        let content = std::mem::take(&mut *buf);
        if let Ok(mut f) = on_event.lock() {
            (*f)(&ComboEvent::Output {
                name: combo_name.to_string(),
                chunk: OutputChunk {
                    timestamp: now_ms(),
                    stream,
                    lines: vec![content],
                },
            });
        }
    }
}

fn format_command_line(command: &str, args: &[String]) -> String {
    let command = resolve_command_display(command);
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_escape(&command));
    for arg in args {
        parts.push(shell_escape(arg));
    }
    parts.join(" ")
}

fn resolve_command_display(command: &str) -> String {
    let command_path = std::path::Path::new(command);
    let workspace_combo_dir = crate::workspace_dir().join(".coco/combos");
    if let Ok(relative) = command_path.strip_prefix(&workspace_combo_dir) {
        let display_path = std::path::Path::new(".coco/combos").join(relative);
        return display_path.to_string_lossy().to_string();
    }
    command.to_string()
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.bytes().all(|byte| {
        matches!(byte, b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'_'
            | b'-'
            | b'.'
            | b'/'
            | b':'
            | b'@'
            | b'+'
            | b'='
            | b','
            | b'%')
    }) {
        return value.to_string();
    }
    let mut escaped = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            escaped.push_str("'\"'\"'");
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('\'');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_run_combo_input() {
        let json = json!({
            "combo_name": "commit"
        });

        let input: RunComboInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.combo_name, "commit");
        assert!(input.args.is_empty());
    }

    #[test]
    fn parse_run_combo_input_args_string() {
        let json = json!({
            "combo_name": "commit",
            "args": "hello world"
        });

        let input: RunComboInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.combo_name, "commit");
        assert_eq!(input.args, vec!["hello world"]);
    }

    #[test]
    fn serialize_run_combo_output() {
        let output = RunComboOutput {
            success: true,
            summary: "Combo completed".to_string(),
            tool_calls: 3,
            error: None,
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["tool_calls"], 3);
        assert!(json.get("error").is_none() || json["error"].is_null());
    }
}
