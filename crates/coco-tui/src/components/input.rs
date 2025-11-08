use color_eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{Frame, prelude::*};

use super::{Action, Component};

pub struct Input<'a> {
    pub textarea: tui_textarea::TextArea<'a>,
    pub cursor_style: Style,
}

impl Default for Input<'_> {
    fn default() -> Self {
        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_cursor_line_style(Style::reset());
        let cursor_style = textarea.cursor_style();
        Self {
            textarea,
            cursor_style,
        }
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
    fn update(&mut self, action: &Action) {
        match action {
            Action::Blur => {
                self.textarea.set_cursor_style(Style::reset());
            }
            Action::Focus => {
                self.textarea.set_cursor_style(self.cursor_style);
            }
            _ => (), // ignore
        }
    }
    fn handle_key_event(&mut self, key: &KeyEvent) {
        self.textarea.input(key.to_owned());
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.textarea, area);
        Ok(())
    }
}
