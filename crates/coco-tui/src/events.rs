use code_combo::{OutputChunk, Starter, TextEdit, ThinkingConfig, ToolUse, tools::Final};
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
    Cancelled,
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
        name: String,
        command_line: String,
    },
    RecordStart {
        name: String,
        tool_use: ToolUse,
    },
    Output {
        name: String,
        chunk: OutputChunk,
    },
    RecordOutput {
        name: String,
        tool_use_id: String,
        chunk: OutputChunk,
    },
    RecordEnd {
        name: String,
        tool_use_id: String,
        is_error: bool,
        output: Final,
    },
    Prompt {
        name: String,
        prompt: String,
        thinking: Option<ThinkingConfig>,
    },
    PromptReply {
        name: String,
        tool_use: ToolUse,
    },
    Executed {
        name: String,
        starter: Starter,
        exit_code: Option<i32>,
    },

    ReplyToolError {
        message: String,
    },

    NotFound {
        name: String,
    },
    Cancelled {
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
