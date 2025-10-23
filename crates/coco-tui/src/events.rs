use code_combo::{Line, Starter};
use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Debug, Clone)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),

    BotMessages(Vec<BotMessage>),
    Combo(ComboEvent),

    Init,
    Tick,
    Render,
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
        Event::Combo(val)
    }
}

#[derive(Debug, Clone)]
pub enum BotMessage {
    Plain(String),
}
