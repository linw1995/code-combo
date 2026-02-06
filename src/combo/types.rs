//! Shared types for combo execution.
//!
//! This module contains types shared between combo tools and combo execution.

use serde::{Deserialize, Serialize};

use crate::{Combo, Message, OutputChunk, ThinkingConfig, ToolUse};

// Import Final from tools module for use in combo events
pub use crate::tools::Final;

/// Stream kind for combo event output.
/// Note: This is different from session::ComboStreamKind which is for socket communication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboEventStreamKind {
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
        tool_use: ToolUse,
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
        /// Session socket path for interactive reply handoff.
        session_sock: Option<String>,
    },
    /// Prompt stream update for combo reply.
    PromptStream {
        /// Combo name.
        name: String,
        /// Stream index.
        index: usize,
        /// Stream kind.
        kind: ComboEventStreamKind,
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
        kind: ComboEventStreamKind,
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
        /// Whether this tool use requires explicit user confirmation in UI.
        requires_confirmation: bool,
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
        starter: crate::Starter,
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
    pub config: crate::Config,
    /// System prompt used for combo reply.
    pub system_prompt: String,
    /// Optional model override for combo reply.
    pub model_override: Option<String>,
    /// Whether thinking is enabled for combo reply.
    pub thinking_enabled: bool,
    /// Whether to ignore workspace combo scripts.
    pub ignore_workspace_scripts: bool,
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

/// Input parameters for combo execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunComboInput {
    /// Name of the combo to execute.
    pub combo_name: String,
    /// Arguments passed to the combo starter.
    #[serde(default)]
    pub args: Vec<String>,
}
