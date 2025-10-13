use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),

    Init,
    Render,
}
