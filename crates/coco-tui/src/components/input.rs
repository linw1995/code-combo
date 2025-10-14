use color_eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{Frame, prelude::*};

use super::Component;

pub struct Input<'a> {
    pub textarea: tui_textarea::TextArea<'a>,
}

impl Default for Input<'_> {
    fn default() -> Self {
        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_cursor_line_style(Style::reset());
        Self { textarea }
    }
}

impl Input<'_> {
    /// Clears all content from the input and returns the deleted text.
    ///
    /// This method selects all text in the textarea, cuts it to clipboard,
    /// and returns the content that was removed.
    pub fn clear(&mut self) -> String {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.yank_text()
    }
}

impl Component for Input<'_> {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        self.textarea.input(key.to_owned());
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.textarea, area);
        Ok(())
    }
}
