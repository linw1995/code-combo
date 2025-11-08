use code_combo::{Line, Starter};
use crossterm::event::{KeyEvent, MouseEvent};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),

    Ask(AskEvent),
    Answer(AnswerEvent),
    Combo(ComboEvent),

    Init,
    Tick,
    Render,
}

#[derive(Debug, Clone)]
pub enum AskEvent {
    Bot,
    // Below events come from Bot
    ToolUsePermission(String),
}

impl From<AskEvent> for Event {
    fn from(value: AskEvent) -> Self {
        Self::Ask(value)
    }
}

#[derive(Debug, Clone)]
pub enum AnswerEvent {
    Bot(Vec<BotMessage>),
    // Below events come from User
    ToolResult { id: String, output: Value },
}

impl From<AnswerEvent> for Event {
    fn from(value: AnswerEvent) -> Self {
        Self::Answer(value)
    }
}

#[derive(Debug, Clone)]
pub enum ComboEvent {
    Discovering,
    Discovered { starters: Vec<Starter> },

    Executing { name: String },
    Output { name: String, lines: Vec<Line> },
    Executed { name: String, starter: Starter },

    NotFound { name: String },
}

impl From<ComboEvent> for Event {
    fn from(val: ComboEvent) -> Self {
        Self::Combo(val)
    }
}

#[derive(Debug, Clone)]
pub enum BotMessage {
    Plain(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}
