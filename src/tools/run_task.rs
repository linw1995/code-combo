//! Run Task Tool - Delegates tasks to configured subagents.
//!
//! This tool allows the main agent to spawn subagent instances
//! that execute specific tasks with their own configuration but
//! inherit permissions from the parent agent.

use std::sync::Arc;

use async_trait::async_trait;
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
}

/// Execute the run_task tool with chunked output support.
pub async fn run_task<F>(
    context: Arc<Mutex<RunTaskContext>>,
    input: Input<'_>,
    cancel_token: CancellationToken,
    mut on_chunk: F,
) -> ExecuteResult
where
    F: FnMut(&OutputChunk) + Send,
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

    // Create subagent instance
    // Clone config and executor for the subagent
    let subagent_base_config = ctx.config.clone();
    let subagent_executor = ctx.executor.clone();

    // Drop lock before async operations
    drop(ctx);

    // Override model if subagent has one configured
    if let Some(ref model) = subagent_agent_config.default_model {
        // TODO: Apply model override to subagent
        debug!(model = %model, "Subagent has custom model configured");
    }

    // Build subagent system prompt
    let subagent_system_prompt = build_subagent_system_prompt(&subagent_agent_config, &input);

    // Create a minimal Agent-like executor for the subagent
    // For now, we'll use a simplified execution loop
    let result = execute_subagent(
        subagent_base_config,
        subagent_executor,
        subagent_system_prompt,
        input.prompt,
        cancel_token,
        &mut on_chunk,
    )
    .await;

    match result {
        Ok(output) => {
            let json_output = serde_json::to_value(&output)
                .unwrap_or_else(|_| json!({"success": false, "error": "Serialization failed"}));
            if output.success {
                Final::from(json_output).ok()
            } else {
                Final::from(json_output).err()
            }
        }
        Err(e) => Final::from(json!({
            "success": false,
            "response": "",
            "turns": 0,
            "error": e
        }))
        .err(),
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
async fn execute_subagent<F>(
    config: Config,
    executor: Executor,
    system_prompt: String,
    initial_prompt: String,
    cancel_token: CancellationToken,
    on_chunk: &mut F,
) -> Result<RunTaskOutput, String>
where
    F: FnMut(&OutputChunk) + Send,
{
    use std::sync::Mutex as StdMutex;

    use crate::agent::Agent;

    // Wrap on_chunk in Mutex for thread-safe interior mutability
    let on_chunk = StdMutex::new(on_chunk);

    // Helper to get current timestamp
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    };

    // Helper to emit stream updates
    let emit_update = |update: ChatStreamUpdate| {
        let chunk = match update {
            ChatStreamUpdate::Plain { text, .. } => OutputChunk {
                timestamp: now_ms(),
                stream: StreamKind::Stdout,
                lines: vec![text],
            },
            ChatStreamUpdate::Thinking { text, .. } => OutputChunk {
                timestamp: now_ms(),
                stream: StreamKind::Stderr,
                lines: vec![format!("[thinking] {}", text)],
            },
        };
        if let Ok(mut f) = on_chunk.lock() {
            (*f)(&chunk);
        }
    };

    // Create subagent with shared executor
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

    while matches!(stop_reason, Some(StopReason::ToolUse)) && turns < max_turns {
        if cancel_token.is_cancelled() {
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
            // Emit tool call start
            {
                let chunk = OutputChunk {
                    timestamp: now_ms(),
                    stream: StreamKind::Stdout,
                    lines: vec![format!("[tool:{}] Starting...", tool_use.name)],
                };
                if let Ok(mut f) = on_chunk.lock() {
                    (*f)(&chunk);
                }
            }

            let input = crate::tools::Input::Starter(tool_use.input.clone());
            let exec_result = executor
                .clone()
                .execute_with_output(
                    &tool_use.id,
                    &tool_use.name,
                    input,
                    cancel_token.clone(),
                    |output| {
                        if let crate::agent::Output::ToolOutput(chunk) = output
                            && let Ok(mut f) = on_chunk.lock()
                        {
                            (*f)(&chunk);
                        }
                    },
                )
                .await;

            let result_content = match exec_result {
                Ok(ExecuteStatus::Completed) => Content::Text("Tool executed successfully".into()),
                Ok(ExecuteStatus::Cancelled) => Content::Text("Tool execution cancelled".into()),
                Err(e) => Content::Text(format!("Tool error: {}", e)),
            };

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
    }

    Ok(RunTaskOutput {
        success: true,
        response: final_response,
        turns,
        error: None,
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
