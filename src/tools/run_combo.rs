//! Combo runner - executes combo scripts within agent context.
//!
//! This module provides combo execution helpers and shared types.
//! Combos are executable scripts that can perform complex operations
//! with recorded tool calls.

use std::{path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    BASH_TOOL_NAME, BashInput, ExecuteResult, Final, Input, Output, prepare_mcp_envs,
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

/// Input parameters for combo execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunComboInput {
    /// Name of the combo to execute.
    pub combo_name: String,
    /// Arguments passed to the combo starter.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Output from combo execution.
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
    /// Optional thinking blocks from the summary response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_thinking: Vec<String>,
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
    /// Reset prompt stream for combo reply.
    PromptStreamReset {
        /// Combo name.
        name: String,
    },
    /// Reset summary stream for combo summary.
    SummaryStreamReset {
        /// Combo name.
        name: String,
    },
    /// Summary stream update for combo summary.
    SummaryStream {
        /// Combo name.
        name: String,
        /// Stream index.
        index: usize,
        /// Stream kind.
        kind: ComboStreamKind,
        /// Streamed text.
        text: String,
    },
    /// Summary completion for combo summary.
    SummaryDone {
        /// Combo name.
        name: String,
        /// Summary text.
        summary: String,
        /// Thinking blocks.
        thinking: Vec<String>,
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
    /// Transcript messages collected during combo execution.
    Transcript {
        /// Combo name.
        name: String,
        /// Transcript messages for the combo reply agent.
        messages: Vec<Message>,
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
    /// Whether to ignore workspace combo scripts.
    pub ignore_workspace_scripts: bool,
}

/// Execute combo logic with event callback support.
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

    let (envs, config, system_prompt, model_override, thinking_enabled, ignore_workspace_scripts) = {
        let ctx = context.lock().await;
        (
            ctx.envs.clone(),
            ctx.config.clone(),
            ctx.system_prompt.clone(),
            ctx.model_override.clone(),
            ctx.thinking_enabled,
            ctx.ignore_workspace_scripts,
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
        let discovered =
            discover_combo_infos(&config, cancel_token.clone(), ignore_workspace_scripts).await;
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
    let mut tool_failed = false;
    let mut starter_failed = false;
    let mut cancelled = false;

    let command_line = format_combo_run_command(&combo_name, &args);
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
                            tool_failed = true;
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
                                            match update {
                                                ChatStreamUpdate::Reset => {
                                                    thinking_seen_stream.store(
                                                        false,
                                                        std::sync::atomic::Ordering::Relaxed,
                                                    );
                                                    emit_combo_event(
                                                        &on_event_stream,
                                                        ComboEvent::PromptStreamReset {
                                                            name: stream_name.clone(),
                                                        },
                                                    );
                                                }
                                                ChatStreamUpdate::Plain { index, text } => {
                                                    emit_combo_event(
                                                        &on_event_stream,
                                                        ComboEvent::PromptStream {
                                                            name: stream_name.clone(),
                                                            index,
                                                            kind: ComboStreamKind::Plain,
                                                            text,
                                                        },
                                                    );
                                                }
                                                ChatStreamUpdate::Thinking { index, text } => {
                                                    thinking_seen_stream.store(
                                                        true,
                                                        std::sync::atomic::Ordering::Relaxed,
                                                    );
                                                    emit_combo_event(
                                                        &on_event_stream,
                                                        ComboEvent::PromptStream {
                                                            name: stream_name.clone(),
                                                            index,
                                                            kind: ComboStreamKind::Thinking,
                                                            text,
                                                        },
                                                    );
                                                }
                                            }
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
                        starter_failed = true;
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
            emit_combo_transcript(&on_event_for_emit, &combo_name, &reply_agent).await;
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
        emit_combo_transcript(&on_event_for_emit, &combo_name, &reply_agent).await;
        return Ok(RunComboOutput {
            success: false,
            summary: "Combo execution was cancelled".to_string(),
            tool_calls,
            error: Some("Cancelled".to_string()),
            summary_thinking: Vec::new(),
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
        emit_combo_transcript(&on_event_for_emit, &combo_name, &reply_agent).await;
        return Ok(RunComboOutput {
            success: false,
            summary: summary_parts.join("\n"),
            tool_calls,
            error: Some(format!("Combo error: {}", e)),
            summary_thinking: Vec::new(),
        });
    }

    let success = !starter_failed && exit_code.map(|c| c == 0).unwrap_or(true);
    let fallback_summary = if summary_parts.is_empty() {
        format!(
            "Combo '{}' completed with {} tool call(s)",
            combo_name, tool_calls
        )
    } else {
        let max_lines = 10;
        let start = summary_parts.len().saturating_sub(max_lines);
        summary_parts[start..].join("\n")
    };
    let (summary, summary_thinking) = if cancel_token.is_cancelled() {
        (fallback_summary, Vec::new())
    } else {
        match generate_combo_summary(
            &mut reply_agent,
            &combo_name,
            tool_calls,
            exit_code,
            tool_failed,
            cancel_token.clone(),
            Some(on_event_for_emit.clone()),
        )
        .await
        {
            Ok(summary) => (summary.summary, summary.thinking),
            Err(err) => {
                warn!(?err, "Failed to generate combo summary");
                (fallback_summary, Vec::new())
            }
        }
    };

    emit_combo_event(
        &on_event_for_emit,
        ComboEvent::SummaryDone {
            name: combo_name.clone(),
            summary: summary.clone(),
            thinking: summary_thinking.clone(),
        },
    );

    emit_combo_transcript(&on_event_for_emit, &combo_name, &reply_agent).await;

    Ok(RunComboOutput {
        success,
        summary,
        tool_calls,
        error: if tool_failed {
            Some("One or more tool calls failed".to_string())
        } else {
            None
        },
        summary_thinking,
    })
}

fn emit_combo_event(on_event: &ComboEventCallback, event: ComboEvent) {
    if let Ok(mut f) = on_event.lock() {
        (*f)(&event);
    }
}

async fn emit_combo_transcript(on_event: &ComboEventCallback, name: &str, agent: &Agent) {
    let messages = agent.dump_messages().await;
    if messages.is_empty() {
        return;
    }
    emit_combo_event(
        on_event,
        ComboEvent::Transcript {
            name: name.to_string(),
            messages,
        },
    );
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

fn extract_text_response(message: &Message) -> String {
    match &message.content {
        Content::Text(text) => text.clone(),
        Content::Multiple(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if let Block::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

struct SummaryResponse {
    summary: String,
    thinking: Vec<String>,
}

async fn generate_combo_summary(
    agent: &mut Agent,
    combo_name: &str,
    tool_calls: usize,
    exit_code: Option<i32>,
    tool_failed: bool,
    cancel_token: CancellationToken,
    on_event: Option<ComboEventCallback>,
) -> Result<SummaryResponse, String> {
    let mut summary_agent = agent.clone();
    summary_agent.apply_tool_policies(Some(&[]), None);
    let prompt = format!(
        "Summarize the combo execution for the user.\n\
- Provide 3-5 concise bullet points.\n\
- Include status (success/failure), key actions, notable outputs or errors, and next steps if any.\n\
- Base the summary on the tool uses, tool results, and prompts already in the conversation history.\n\
- Do not call tools.\n\
\n\
Context:\n\
combo_name: {combo_name}\n\
tool_calls: {tool_calls}\n\
exit_code: {exit_code}\n\
tool_failed: {tool_failed}\n\
",
        exit_code = exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    let disable_stream = summary_agent.disable_stream_for_current_model();
    let response = if disable_stream || on_event.is_none() {
        summary_agent
            .chat(Message::user(Content::Text(prompt)))
            .await
            .map_err(|err| err.to_string())?
    } else {
        let stream_name = combo_name.to_string();
        if let Some(on_event) = on_event.as_ref() {
            emit_combo_event(
                on_event,
                ComboEvent::SummaryStreamReset {
                    name: stream_name.clone(),
                },
            );
        }
        let on_event_stream = on_event.clone();
        summary_agent
            .chat_stream(
                Message::user(Content::Text(prompt)),
                cancel_token,
                move |update| {
                    let Some(on_event_stream) = on_event_stream.as_ref() else {
                        return;
                    };
                    match update {
                        ChatStreamUpdate::Reset => emit_combo_event(
                            on_event_stream,
                            ComboEvent::SummaryStreamReset {
                                name: stream_name.clone(),
                            },
                        ),
                        ChatStreamUpdate::Plain { index, text } => emit_combo_event(
                            on_event_stream,
                            ComboEvent::SummaryStream {
                                name: stream_name.clone(),
                                index,
                                kind: ComboStreamKind::Plain,
                                text,
                            },
                        ),
                        ChatStreamUpdate::Thinking { index, text } => emit_combo_event(
                            on_event_stream,
                            ComboEvent::SummaryStream {
                                name: stream_name.clone(),
                                index,
                                kind: ComboStreamKind::Thinking,
                                text,
                            },
                        ),
                    }
                },
            )
            .await
            .map_err(|err| err.to_string())?
    };
    let summary = extract_text_response(&response.message);
    let summary = summary.trim();
    if summary.is_empty() {
        return Err("summary response is empty".to_string());
    }
    let thinking = extract_thinking_blocks(&response.message);
    Ok(SummaryResponse {
        summary: summary.to_string(),
        thinking,
    })
}

fn extract_thinking_blocks(message: &Message) -> Vec<String> {
    match &message.content {
        Content::Multiple(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Block::Thinking { thinking, .. } if !thinking.is_empty() => Some(thinking.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
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
        .chat_stream_with_history(cancel_token.clone(), move |update| match update {
            ChatStreamUpdate::Reset => {
                emit_combo_event(
                    &on_event_stream,
                    ComboEvent::PromptStreamReset {
                        name: stream_name.clone(),
                    },
                );
            }
            ChatStreamUpdate::Plain { index, text } => {
                emit_combo_event(
                    &on_event_stream,
                    ComboEvent::PromptStream {
                        name: stream_name.clone(),
                        index,
                        kind: ComboStreamKind::Plain,
                        text,
                    },
                );
            }
            ChatStreamUpdate::Thinking { index, text } => {
                emit_combo_event(
                    &on_event_stream,
                    ComboEvent::PromptStream {
                        name: stream_name.clone(),
                        index,
                        kind: ComboStreamKind::Thinking,
                        text,
                    },
                );
            }
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
    ignore_workspace_scripts: bool,
) -> ComboDiscoveryResult {
    let combo_dirs = combo_discovery_dirs(config, ignore_workspace_scripts);
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

fn combo_discovery_dirs(config: &Config, ignore_workspace_scripts: bool) -> Vec<PathBuf> {
    let mut combo_dirs = Vec::with_capacity(2);
    if !ignore_workspace_scripts {
        combo_dirs.push(workspace_dir().join(".coco/combos"));
    }
    combo_dirs.push(config.combo_dir());
    combo_dirs
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

fn format_combo_run_command(name: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 4);
    parts.push("coco".to_string());
    parts.push("combo".to_string());
    parts.push("run".to_string());
    parts.push(display_combo_arg(name));
    for arg in args {
        parts.push(display_combo_arg(arg));
    }
    parts.join(" ")
}

fn display_combo_arg(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value.bytes().all(|byte| {
        matches!(byte, b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'_'
            | b'-'
            | b'.'
            | b'/'
            | b':')
    }) {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_run_combo_output() {
        let output = RunComboOutput {
            success: true,
            summary: "Combo completed".to_string(),
            tool_calls: 3,
            error: None,
            summary_thinking: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["tool_calls"], 3);
        assert!(json.get("error").is_none() || json["error"].is_null());
    }
}
