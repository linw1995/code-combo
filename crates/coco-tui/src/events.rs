use code_combo::{
    OutputChunk, Starter, TextEdit, ThinkingConfig, ToolUse, UsageStats,
    tools::{ComboEvent as ComboToolEvent, Final, SubagentEvent},
};
use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Debug, Clone)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),

    Ask(AskEvent),
    Answer(AnswerEvent),
    Combo(ComboEvent),

    Init,
    Tick,
    Dirty,
    Render,
    FullRefresh,
}

#[derive(Debug, Clone)]
pub enum AskEvent {
    Bot,
    // Below events come from Bot
    ToolUsePermission(String),
    TextEdit {
        id: String,
        edit: TextEdit,
        auto_accept: bool,
    },
}

impl From<AskEvent> for Event {
    fn from(value: AskEvent) -> Self {
        Self::Ask(value)
    }
}

#[derive(Debug, Clone)]
pub enum AnswerEvent {
    Bot(Vec<BotMessage>),
    BotStreamReset,
    BotStream {
        index: usize,
        kind: BotStreamKind,
        text: String,
    },
    Cancelled,
    Usage {
        usage: UsageStats,
    },
    // Below events come from User
    ToolOutput {
        id: String,
        chunk: OutputChunk,
    },
    ToolResult {
        id: String,
        is_error: bool,
        is_user_cancelled: bool,
        output: Final,
    },
    /// Subagent event (for run_task tool).
    SubagentEvent {
        id: String,
        event: SubagentEvent,
    },
    /// Combo tool event (for run_combo tool).
    ComboToolEvent {
        id: String,
        event: ComboToolEvent,
    },
}

impl From<AnswerEvent> for Event {
    fn from(value: AnswerEvent) -> Self {
        Self::Answer(value)
    }
}

#[derive(Debug, Clone)]
pub enum ComboEvent {
    Discovering,
    Discovered {
        starters: Vec<Starter>,
    },

    Executing {
        id: String,
        name: String,
        command_line: String,
    },
    RecordStart {
        id: String,
        name: String,
        tool_use: ToolUse,
    },
    Output {
        id: String,
        name: String,
        chunk: OutputChunk,
    },
    RecordOutput {
        id: String,
        name: String,
        tool_use_id: String,
        chunk: OutputChunk,
    },
    RecordEnd {
        id: String,
        name: String,
        tool_use_id: String,
        is_error: bool,
        output: Final,
    },
    Prompt {
        id: String,
        name: String,
        prompt: String,
        thinking: Option<ThinkingConfig>,
    },
    PromptStream {
        id: String,
        name: String,
        index: usize,
        kind: BotStreamKind,
        text: String,
    },
    /// Reply tool use from prompt, with optional offload via bash
    ReplyToolUse {
        id: String,
        name: String,
        tool_use: ToolUse,
        thinking: Vec<String>,
        /// Whether this is an offload reply (executed via bash)
        offload: bool,
    },
    /// Result of offload reply bash execution
    ReplyToolResult {
        id: String,
        name: String,
        tool_use_id: String,
        is_error: bool,
        output: Final,
    },
    Executed {
        id: String,
        name: String,
        starter: Starter,
        exit_code: Option<i32>,
    },

    ReplyToolError {
        message: String,
    },

    NotFound {
        id: String,
        name: String,
    },
    Cancelled {
        id: Option<String>,
        name: Option<String>,
    },
}

impl From<ComboEvent> for Event {
    fn from(val: ComboEvent) -> Self {
        Self::Combo(val)
    }
}

#[derive(Debug, Clone)]
pub enum BotMessage {
    Plain(String),
    ToolUse(ToolUse),
    System(String),
    Thinking(String),
}

#[derive(Debug, Clone, Copy)]
pub enum BotStreamKind {
    Plain,
    Thinking,
}
