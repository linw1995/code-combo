use color_eyre::eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{Frame, prelude::*};

use super::{Action, Component};

#[derive(Default)]
#[allow(dead_code)]
pub struct Input<'a> {
    pub textarea: tui_textarea::TextArea<'a>,
}

impl Component for Input<'_> {
    fn handle_key_events(&mut self, key: KeyEvent) -> Action {
        self.textarea.input(key);
        Action::Noop
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.textarea, area);
        Ok(())
    }
}
