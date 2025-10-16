use code_combo::Starter;
use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Debug, Clone)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),

    Combo(ComboEvent),

    Init,
    Render,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ComboEvent {
    Discovering,
    Discovered { starters: Vec<Starter> },

    Executing { name: String },
    Executed { name: String, starter: Starter },

    NotFound { name: String },
}

impl From<ComboEvent> for Event {
    fn from(val: ComboEvent) -> Self {
        Event::Combo(val)
    }
}
