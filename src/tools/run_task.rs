//! Run Task Tool - Delegates tasks to configured subagents.
//!
//! This tool allows the main agent to spawn subagent instances
//! that execute specific tasks with their own configuration but
//! inherit permissions from the parent agent.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{ExecuteResult, Final, Input, Tool};
use crate::{
    Config, OutputChunk,
    agent::{
        AgentConfig, ChatStreamUpdate, Content, ExecuteStatus, Executor, Message, StopReason,
        SubagentConfig,
    },
    exec::StreamKind,
    provider::{Block, ToolUse},
};

pub const RUN_TASK_TOOL_NAME: &str = "run_task";

type SubagentEventCallback = Arc<std::sync::Mutex<Box<dyn FnMut(&SubagentEvent) + Send>>>;

/// Input parameters for the run_task tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTaskInput {
    /// Name of the subagent to invoke.
    pub subagent_name: String,
    /// Short description of the task (3-5 words).
    pub description: String,
    /// Detailed prompt/instructions for the subagent.
    pub prompt: String,
}

/// Output from the run_task tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTaskOutput {
    /// Whether the task completed successfully.
    pub success: bool,
    /// The subagent's response.
    pub response: String,
    /// Number of turns the subagent took.
    pub turns: usize,
    /// Optional error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Status of a tool execution within the subagent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Tool is starting execution.
    Starting,
    /// Tool is currently executing.
    Executing,
    /// Tool completed successfully.
    Completed,
    /// Tool execution failed.
    Failed,
    /// Tool execution was cancelled.
    Cancelled,
}

/// Event emitted during subagent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentEvent {
    /// Regular output chunk (stdout/stderr).
    Output(OutputChunk),
    /// Tool execution status update.
    ToolUse {
        /// Tool use ID.
        id: String,
        /// Tool name.
        name: String,
        /// Current status.
        status: ToolStatus,
        /// Brief input summary (for Starting/Executing).
        #[serde(skip_serializing_if = "Option::is_none")]
        input_summary: Option<String>,
        /// Brief output summary (for Completed/Failed).
        #[serde(skip_serializing_if = "Option::is_none")]
        output_summary: Option<String>,
    },
    /// Permission request for tool execution.
    AskPermission {
        /// Tool use ID.
        id: String,
        /// Tool name.
        name: String,
        /// Brief input summary.
        #[serde(skip_serializing_if = "Option::is_none")]
        input_summary: Option<String>,
    },
}

/// Configuration needed to create and run subagent instances.
#[derive(Clone)]
pub struct RunTaskContext {
    /// Available subagent configurations.
    pub subagents: Vec<SubagentConfig>,
    /// Base config for creating agents.
    pub config: Config,
    /// Shared executor for permission inheritance.
    pub executor: Executor,
}

/// Tool for delegating tasks to subagents.
pub struct RunTaskTool {
    context: Arc<Mutex<RunTaskContext>>,
}

impl RunTaskTool {
    /// Create a new RunTaskTool with the given context.
    pub fn new(context: RunTaskContext) -> Self {
        Self {
            context: Arc::new(Mutex::new(context)),
        }
    }

    /// Get the shared context for streaming execution.
    pub fn context(&self) -> Arc<Mutex<RunTaskContext>> {
        self.context.clone()
    }

    /// Build tool description including available subagents.
    #[allow(dead_code)]
    fn build_description(subagents: &[SubagentConfig]) -> String {
        let mut desc = String::from(
            "Delegate a task to a specialized subagent. \
             The subagent will execute the task autonomously and return the result.\n\n\
             Available subagents:\n",
        );
        for subagent in subagents {
            desc.push_str(&format!("- {}", subagent.name));
            if let Some(ref d) = subagent.description {
                desc.push_str(&format!(": {}", d));
            }
            desc.push('\n');
        }
        desc
    }
}

#[async_trait]
impl Tool for RunTaskTool {
    fn name(&self) -> &'static str {
        RUN_TASK_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Delegate a task to a specialized subagent for autonomous execution."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_name": {
                    "type": "string",
                    "description": "Name of the subagent to invoke"
                },
                "description": {
                    "type": "string",
                    "description": "Short description of the task (3-5 words)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Detailed instructions for the subagent"
                }
            },
            "required": ["subagent_name", "description", "prompt"]
        })
    }

    async fn execute<'a>(&self, input: Input<'a>) -> ExecuteResult {
        run_task(
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

/// Execute the run_task tool with subagent event support.
pub async fn run_task<F>(
    context: Arc<Mutex<RunTaskContext>>,
    input: Input<'_>,
    cancel_token: CancellationToken,
    on_event: F,
) -> ExecuteResult
where
    F: FnMut(&SubagentEvent) + Send + 'static,
{
    let Input::Starter(input) = input else {
        return err_msg!("Input should be Starter variant");
    };

    let input: RunTaskInput = match serde_json::from_value(input) {
        Ok(v) => v,
        Err(e) => {
            return Final::from(json!({
                "success": false,
                "response": "",
                "turns": 0,
                "error": format!("Invalid input: {}", e)
            }))
            .err();
        }
    };

    debug!(
        subagent = %input.subagent_name,
        description = %input.description,
        "Starting subagent task"
    );

    let ctx = context.lock().await;

    // Find subagent config
    let subagent_config = ctx.subagents.iter().find(|s| s.name == input.subagent_name);

    let Some(subagent_config) = subagent_config else {
        let available: Vec<_> = ctx.subagents.iter().map(|s| s.name.as_str()).collect();
        return Final::from(json!({
            "success": false,
            "response": "",
            "turns": 0,
            "error": format!(
                "Subagent '{}' not found. Available: {:?}",
                input.subagent_name, available
            )
        }))
        .err();
    };

    // Load subagent's agent config (from file or inline)
    let subagent_agent_config = if let Some(ref path) = subagent_config.path {
        // Load from file
        match AgentConfig::try_from_file(path) {
            Ok(Some(config)) => config,
            Ok(None) => {
                return Final::from(json!({
                    "success": false,
                    "response": "",
                    "turns": 0,
                    "error": format!(
                        "Subagent config file not found: {}",
                        path.display()
                    )
                }))
                .err();
            }
            Err(e) => {
                return Final::from(json!({
                    "success": false,
                    "response": "",
                    "turns": 0,
                    "error": format!("Failed to load subagent config: {}", e)
                }))
                .err();
            }
        }
    } else {
        // Use inline config from SubagentConfig
        AgentConfig {
            name: Some(subagent_config.name.clone()),
            description: subagent_config.description.clone(),
            system_prompt: subagent_config.system_prompt.as_ref().map(|content| {
                crate::agent::SystemPromptConfig::Inline {
                    content: content.clone(),
                    args: None,
                }
            }),
            tools: subagent_config.tools.clone(),
            ..Default::default()
        }
    };

    // Clone config and executor for the subagent
    let subagent_base_config = ctx.config.clone();
    let mut parent_executor = ctx.executor.clone();

    // Drop lock before async operations
    drop(ctx);

    // Apply tool restrictions to the parent executor (for permission checks)
    if let Some(ref tools) = subagent_agent_config.tools {
        debug!(tools = ?tools, "Applying tool restrictions for subagent");
        parent_executor.apply_tool_policies(Some(tools), None);
    }

    // Override model if subagent has one configured
    if let Some(ref model) = subagent_agent_config.default_model {
        // TODO: Apply model override to subagent
        debug!(model = %model, "Subagent has custom model configured");
    }

    // Build subagent system prompt
    let subagent_system_prompt = build_subagent_system_prompt(&subagent_agent_config, &input);

    // Wrap callback in Arc<Mutex<Box>> to break type recursion in execute_subagent
    let on_event_boxed: SubagentEventCallback = Arc::new(std::sync::Mutex::new(Box::new(on_event)));

    // Execute subagent with parent executor (for permission inheritance)
    let result = execute_subagent(
        subagent_base_config,
        parent_executor,
        subagent_system_prompt,
        input.prompt,
        cancel_token,
        on_event_boxed,
    )
    .await;

    match result {
        Ok(output) => {
            debug!(
                success = output.success,
                turns = output.turns,
                response_len = output.response.len(),
                "run_task completed, returning result to main agent"
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
            debug!(error = %e, "run_task failed");
            Final::from(json!({
                "success": false,
                "response": "",
                "turns": 0,
                "error": e
            }))
            .err()
        }
    }
}

fn build_subagent_system_prompt(config: &AgentConfig, input: &RunTaskInput) -> String {
    use crate::agent::SystemPromptConfig;

    let base_prompt = match &config.system_prompt {
        Some(SystemPromptConfig::Inline { content, .. }) => content.clone(),
        Some(SystemPromptConfig::File { path, .. }) => {
            std::fs::read_to_string(path).unwrap_or_else(|_| String::new())
        }
        None => String::new(),
    };

    format!(
        "{}\n\n## Current Task\n\n**Description**: {}\n\n{}",
        base_prompt, input.description, input.prompt
    )
}

/// Execute the subagent loop.
///
/// Returns a boxed future to break type-level recursion that would otherwise
/// cause compiler recursion limit issues.
fn execute_subagent(
    config: Config,
    executor: Executor,
    system_prompt: String,
    initial_prompt: String,
    cancel_token: CancellationToken,
    on_event: SubagentEventCallback,
) -> BoxFuture<'static, Result<RunTaskOutput, String>> {
    use crate::agent::Agent;

    Box::pin(async move {
        // Clone executor for tool execution (already has tool restrictions applied)
        let mut tool_executor = executor;
        // Helper to get current timestamp
        let now_ms = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        };

        // Clone Arc for closures
        let on_event_for_emit = on_event.clone();
        let on_event_for_update = on_event.clone();

        // Helper to emit events
        let emit_event = move |event: SubagentEvent| {
            if let Ok(mut f) = on_event_for_emit.lock() {
                (*f)(&event);
            }
        };

        // Shared buffers for aggregating streaming text into lines
        let plain_buffer = Arc::new(std::sync::Mutex::new(String::new()));
        let thinking_buffer = Arc::new(std::sync::Mutex::new(String::new()));

        // Clone buffers for the emit_update closure
        let plain_buf_for_emit = plain_buffer.clone();
        let thinking_buf_for_emit = thinking_buffer.clone();

        // Helper to emit stream updates as Output events, aggregating into lines
        let emit_update = move |update: ChatStreamUpdate| {
            let (buffer, stream, prefix) = match &update {
                ChatStreamUpdate::Plain { .. } => (&plain_buf_for_emit, StreamKind::Stdout, ""),
                ChatStreamUpdate::Thinking { .. } => {
                    (&thinking_buf_for_emit, StreamKind::Stderr, "[thinking] ")
                }
            };

            let text = match update {
                ChatStreamUpdate::Plain { text, .. } => text,
                ChatStreamUpdate::Thinking { text, .. } => text,
            };

            let mut buf = buffer.lock().unwrap();
            buf.push_str(&text);

            // Extract complete lines (ending with newline)
            let mut lines_to_emit = Vec::new();
            while let Some(newline_pos) = buf.find('\n') {
                let line = buf.drain(..=newline_pos).collect::<String>();
                let line = line.trim_end_matches('\n').to_string();
                if !line.is_empty() || prefix.is_empty() {
                    lines_to_emit.push(format!("{}{}", prefix, line));
                }
            }

            // Emit complete lines
            if !lines_to_emit.is_empty() {
                let chunk = OutputChunk {
                    timestamp: now_ms(),
                    stream,
                    lines: lines_to_emit,
                };
                if let Ok(mut f) = on_event_for_update.lock() {
                    (*f)(&SubagentEvent::Output(chunk));
                }
            }
        };

        // Helper to flush remaining buffered content
        let flush_buffers = || {
            let mut lines_to_emit = Vec::new();

            // Flush plain buffer
            if let Ok(mut buf) = plain_buffer.lock()
                && !buf.is_empty()
            {
                lines_to_emit.push((StreamKind::Stdout, std::mem::take(&mut *buf)));
            }

            // Flush thinking buffer
            if let Ok(mut buf) = thinking_buffer.lock()
                && !buf.is_empty()
            {
                let content = std::mem::take(&mut *buf);
                lines_to_emit.push((StreamKind::Stderr, format!("[thinking] {}", content)));
            }

            // Emit remaining content
            for (stream, line) in lines_to_emit {
                if let Ok(mut f) = on_event.lock() {
                    (*f)(&SubagentEvent::Output(OutputChunk {
                        timestamp: now_ms(),
                        stream,
                        lines: vec![line],
                    }));
                }
            }
        };

        // Create subagent and configure it
        let mut subagent = Agent::new(config);
        subagent.set_system_prompt(&system_prompt);

        // Send initial message
        let user_message = Message::user(Content::Text(initial_prompt));

        let mut turns = 0;
        let max_turns = 50; // Prevent infinite loops

        // Initial chat
        let response = subagent
            .chat_stream(user_message, cancel_token.clone(), &emit_update)
            .await
            .map_err(|e| format!("Subagent chat failed: {}", e))?;

        turns += 1;
        let mut final_response = extract_text_response(&response.message);

        // Check if we need tool execution
        let mut stop_reason = response.stop_reason;
        let mut current_message = response.message;

        debug!(?stop_reason, turns, "Subagent initial response");

        while matches!(stop_reason, Some(StopReason::ToolUse)) && turns < max_turns {
            if cancel_token.is_cancelled() {
                flush_buffers();
                return Ok(RunTaskOutput {
                    success: false,
                    response: final_response,
                    turns,
                    error: Some("Cancelled".to_string()),
                });
            }

            // Extract and execute tool calls
            let tool_uses = extract_tool_uses(&current_message);

            if tool_uses.is_empty() {
                break;
            }

            // Execute each tool and collect results
            let mut tool_results = Vec::new();
            for tool_use in tool_uses {
                // Generate input summary for display
                let input_summary = summarize_tool_input(&tool_use.name, &tool_use.input);

                // Emit tool call start
                emit_event(SubagentEvent::ToolUse {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    status: ToolStatus::Starting,
                    input_summary: Some(input_summary.clone()),
                    output_summary: None,
                });

                // Clone info for the execute_with_output callback
                let on_event_for_exec = on_event.clone();
                let tool_id_for_perm = tool_use.id.clone();
                let tool_name_for_perm = tool_use.name.clone();
                let input_summary_for_perm = input_summary.clone();
                let permission_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let permission_requested_clone = permission_requested.clone();

                // Capture tool output result
                let tool_output_result: Arc<std::sync::Mutex<Option<(Content, bool)>>> =
                    Arc::new(std::sync::Mutex::new(None));
                let tool_output_clone = tool_output_result.clone();

                let input = crate::tools::Input::Starter(tool_use.input.clone());
                let exec_result = tool_executor
                    .execute_with_output(
                        &tool_use.id,
                        &tool_use.name,
                        input,
                        cancel_token.clone(),
                        |output| match &output {
                            crate::agent::Output::ToolOutput(chunk) => {
                                if let Ok(mut f) = on_event_for_exec.lock() {
                                    (*f)(&SubagentEvent::Output(chunk.clone()));
                                }
                            }
                            crate::agent::Output::AskPermission => {
                                permission_requested_clone
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                if let Ok(mut f) = on_event_for_exec.lock() {
                                    (*f)(&SubagentEvent::AskPermission {
                                        id: tool_id_for_perm.clone(),
                                        name: tool_name_for_perm.clone(),
                                        input_summary: Some(input_summary_for_perm.clone()),
                                    });
                                }
                            }
                            crate::agent::Output::Success(final_output)
                            | crate::agent::Output::Failure(final_output) => {
                                let is_success = matches!(output, crate::agent::Output::Success(_));
                                let content = match final_output {
                                    crate::tools::Final::Json(v) => Content::Text(v.to_string()),
                                    crate::tools::Final::Message(t) => Content::Text(t.clone()),
                                };
                                if let Ok(mut guard) = tool_output_clone.lock() {
                                    *guard = Some((content, is_success));
                                }
                            }
                            _ => {}
                        },
                    )
                    .await;

                // Check if permission was requested (tool not actually executed)
                let needs_permission =
                    permission_requested.load(std::sync::atomic::Ordering::SeqCst);

                // Get captured tool output
                let captured_output = tool_output_result.lock().ok().and_then(|g| g.clone());

                let (result_content, status, output_summary) = if needs_permission {
                    (
                        Content::Text("Permission required for this operation".into()),
                        ToolStatus::Failed,
                        Some("Permission required".to_string()),
                    )
                } else if let Some((content, is_success)) = captured_output {
                    (
                        content,
                        if is_success {
                            ToolStatus::Completed
                        } else {
                            ToolStatus::Failed
                        },
                        Some(if is_success {
                            "Success".to_string()
                        } else {
                            "Failed".to_string()
                        }),
                    )
                } else {
                    match exec_result {
                        Ok(ExecuteStatus::Completed) => (
                            Content::Text("Tool executed successfully".into()),
                            ToolStatus::Completed,
                            Some("Success".to_string()),
                        ),
                        Ok(ExecuteStatus::Cancelled) => (
                            Content::Text("Tool execution cancelled".into()),
                            ToolStatus::Cancelled,
                            Some("Cancelled".to_string()),
                        ),
                        Err(ref e) => (
                            Content::Text(format!("Tool error: {}", e)),
                            ToolStatus::Failed,
                            Some(format!("Error: {}", e)),
                        ),
                    }
                };

                // Emit tool call end
                emit_event(SubagentEvent::ToolUse {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    status,
                    input_summary: Some(input_summary),
                    output_summary,
                });

                tool_results.push(Block::tool_result(&tool_use.id, None, result_content));
            }

            // Send tool results back to subagent
            subagent
                .append_message(Message::user(Content::Multiple(tool_results)))
                .await;

            // Continue conversation
            let next_response = subagent
                .chat_stream_with_history(cancel_token.clone(), &emit_update)
                .await
                .map_err(|e| format!("Subagent continuation failed: {}", e))?;

            turns += 1;
            final_response = extract_text_response(&next_response.message);
            stop_reason = next_response.stop_reason;
            current_message = next_response.message;

            debug!(?stop_reason, turns, "Subagent continuation response");
        }

        debug!(
            turns,
            response_len = final_response.len(),
            "Subagent execution completed, exiting loop"
        );

        // Flush any remaining buffered content before returning
        flush_buffers();

        Ok(RunTaskOutput {
            success: true,
            response: final_response,
            turns,
            error: None,
        })
    })
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

fn extract_tool_uses(message: &Message) -> Vec<ToolUse> {
    match &message.content {
        Content::Text(_) => Vec::new(),
        Content::Multiple(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if let Block::ToolUse(tool_use) = block {
                    Some(tool_use.clone())
                } else {
                    None
                }
            })
            .collect(),
    }
}

/// Generate a brief summary of tool input for display.
fn summarize_tool_input(tool_name: &str, input: &Value) -> String {
    const MAX_LEN: usize = 60;

    match tool_name {
        "bash" => {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                truncate_str(cmd, MAX_LEN)
            } else {
                "(no command)".to_string()
            }
        }
        "read" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                truncate_str(path, MAX_LEN)
            } else {
                "(no path)".to_string()
            }
        }
        "list" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                truncate_str(path, MAX_LEN)
            } else {
                ".".to_string()
            }
        }
        "str_replace" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                truncate_str(path, MAX_LEN)
            } else {
                "(no path)".to_string()
            }
        }
        _ => {
            // For unknown tools, show first key-value or truncated JSON
            if let Some(obj) = input.as_object() {
                if let Some((key, val)) = obj.iter().next() {
                    let val_str = match val {
                        Value::String(s) => truncate_str(s, 40),
                        _ => val.to_string(),
                    };
                    format!(
                        "{}: {}",
                        key,
                        truncate_str(&val_str, MAX_LEN - key.len() - 2)
                    )
                } else {
                    "{}".to_string()
                }
            } else {
                truncate_str(&input.to_string(), MAX_LEN)
            }
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    // Take first line only
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_run_task_input() {
        let json = json!({
            "subagent_name": "coder",
            "description": "Fix the bug",
            "prompt": "Please fix the null pointer exception in main.rs"
        });

        let input: RunTaskInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.subagent_name, "coder");
        assert_eq!(input.description, "Fix the bug");
        assert!(input.prompt.contains("null pointer"));
    }

    #[test]
    fn serialize_run_task_output() {
        let output = RunTaskOutput {
            success: true,
            response: "Task completed".to_string(),
            turns: 3,
            error: None,
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["turns"], 3);
        assert!(json.get("error").is_none() || json["error"].is_null());
    }
}
